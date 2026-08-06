// The README's examples are compiled and run as doctests, so they cannot drift out of
// sync with the API.
#![doc = include_str!("../README.md")]

use bytes::Bytes;
use memmap2::MmapMut;
use std::fs::OpenOptions;
use std::hint;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::time::{Duration, Instant};

/// "MINICACH", so we refuse to map a file that is not ours.
const MAGIC: u64 = 0x4d49_4e49_4341_4348;
const FORMAT_VERSION: u32 = 1;

/// Largest key we will store. Keys are inline in the slot, not indirected.
pub const MAX_KEY_LEN: usize = 64;
/// Largest value we will store. Sized for the sub-1KB values this cache targets.
pub const MAX_VAL_LEN: usize = 1024;

/// Busy-spin attempts before falling back to yielding. Covers the common case where
/// the slot holder is running on another core and will release within nanoseconds.
const SPIN_ATTEMPTS: usize = 64;

/// How long a writer waits for a contended slot before declaring the holder dead.
///
/// This must be a wall-clock bound, not an iteration count. A live writer can be
/// preempted mid-write for milliseconds, and an iteration count cannot distinguish
/// that from a writer that died — which would make ordinary multi-writer contention
/// fail writes at random.
const WRITE_LOCK_TIMEOUT: Duration = Duration::from_millis(50);

/// How long a reader waits before treating a locked slot as a miss. Shorter than the
/// writer's bound because a reader has the option of giving up: a spurious miss is
/// valid cache behaviour, whereas a spurious write failure loses data.
const READ_LOCK_TIMEOUT: Duration = Duration::from_millis(5);

/// Spins for `SPIN_ATTEMPTS`, then yields, and reports whether `timeout` has elapsed.
///
/// The clock is only consulted once the spin phase is exhausted, so an uncontended
/// slot never pays for a `Instant::now()` call.
struct Backoff {
    attempts: usize,
    deadline: Option<Instant>,
}

impl Backoff {
    fn new() -> Self {
        Backoff {
            attempts: 0,
            deadline: None,
        }
    }

    /// Returns `false` once `timeout` has elapsed, meaning the holder is presumed dead.
    fn wait(&mut self, timeout: Duration) -> bool {
        self.attempts += 1;
        if self.attempts <= SPIN_ATTEMPTS {
            hint::spin_loop();
            return true;
        }
        let deadline = *self.deadline.get_or_insert_with(|| Instant::now() + timeout);
        if Instant::now() >= deadline {
            return false;
        }
        // Hand the core over: the holder may be a descheduled process on this CPU,
        // in which case spinning actively prevents it from making progress.
        std::thread::yield_now();
        true
    }
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    KeyTooLong { len: usize },
    ValueTooLong { len: usize },
    /// The file exists but was not written by this format/version.
    BadFormat,
    /// A slot stayed write-locked for `SPIN_LIMIT` iterations, which in practice
    /// means the process that locked it died before unlocking.
    SlotStalled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::KeyTooLong { len } => {
                write!(f, "key of {len} bytes exceeds limit of {MAX_KEY_LEN}")
            }
            Error::ValueTooLong { len } => {
                write!(f, "value of {len} bytes exceeds limit of {MAX_VAL_LEN}")
            }
            Error::BadFormat => write!(f, "file is not a minicache shared-memory cache"),
            Error::SlotStalled => write!(f, "slot is locked by a writer that never finished"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Written once by whichever process wins the initialisation race.
#[repr(C, align(64))]
struct Header {
    magic: AtomicU64,
    version: AtomicU32,
    num_slots: AtomicU32,
    /// Published last, so a process that sees `ready == 1` sees a complete header.
    ready: AtomicU32,
    _pad: [u8; 44],
}

/// `seq` even means stable, odd means a writer holds the slot.
///
/// A zeroed slot is naturally valid: `seq == 0` (even, unlocked) and `key_len == 0`,
/// which matches no key, so a freshly created file reads as empty.
#[repr(C, align(64))]
struct Slot {
    seq: AtomicU64,
    key_len: u32,
    val_len: u32,
    key: [u8; MAX_KEY_LEN],
    val: [u8; MAX_VAL_LEN],
}

/// A cache that lives in a memory-mapped file, so several processes on the same host
/// share one cache with no server and no network hop.
///
/// A handle is obtained with [`ShmCache::open`]; any process opening the same path
/// attaches to the same cache, and whichever gets there first creates it.
///
/// # Layout
///
/// Everything inside the mapping is `#[repr(C)]` with a fixed layout. Nothing in there
/// may be a pointer: each process maps the file at a different address, so a pointer
/// written by one process is meaningless to another. This is why the cache cannot be
/// built on `HashMap`/`Vec`, whose contents are process-local heap.
///
/// ```text
/// ┌──────────────────────────────────────┐
/// │ Header   (one cacheline)             │
/// │   magic, version, num_slots, ready   │
/// ├──────────────────────────────────────┤
/// │ Slots    [Slot; num_slots]           │
/// │   seq, key_len, val_len, key, val    │
/// └──────────────────────────────────────┘
/// ```
///
/// Slots are direct-mapped by hash. A collision overwrites, which is ordinary cache
/// behaviour, so there is no LRU ordering — maintaining one across processes needs
/// shared locking, which is exactly what this design avoids.
///
/// # Concurrency
///
/// Each slot is a seqlock. A writer bumps `seq` to odd, writes, then stores it even
/// again; a reader samples `seq`, copies the value out, and re-reads `seq`, retrying if
/// it moved. The copy is what makes the re-read meaningful — see [`ShmCache::read`].
///
/// There are no process-shared mutexes, so a process dying mid-write cannot poison or
/// deadlock the cache. It leaves one slot with an odd `seq`, which both sides wait on
/// for a bounded time, so that slot degrades to a miss rather than wedging a caller
/// forever. Readers never write to shared memory at all, so a crashed reader cannot
/// damage anything.
pub struct ShmCache {
    mmap: MmapMut,
    num_slots: usize,
}

// The mapping is shared across processes and every access goes through the slot
// seqlock, so a shared reference is enough to write.
unsafe impl Send for ShmCache {}
unsafe impl Sync for ShmCache {}

impl ShmCache {
    /// Open the cache at `path`, creating and initialising it if it does not exist.
    ///
    /// `num_slots` is only honoured by whichever process creates the file. An existing
    /// cache keeps the slot count it was created with, and the caller's value is
    /// ignored — reinterpreting someone else's slots at a different modulus would mean
    /// the two processes hash the same key to different places and silently stop
    /// sharing. Use [`ShmCache::capacity`] to see what you actually got.
    pub fn open<P: AsRef<Path>>(path: P, num_slots: usize) -> Result<Self, Error> {
        assert!(num_slots > 0, "num_slots must be > 0");
        assert!(
            num_slots <= u32::MAX as usize,
            "num_slots must fit in a u32"
        );

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        // Only ever grow. `set_len` to a smaller size would truncate a cache another
        // process is actively using.
        if file.metadata()?.len() < Self::file_len(num_slots) as u64 {
            file.set_len(Self::file_len(num_slots) as u64)?;
        }

        let mut mmap = unsafe { MmapMut::map_mut(&file)? };
        if mmap.len() < size_of::<Header>() {
            return Err(Error::BadFormat);
        }

        let effective = Self::init_header(&mmap, num_slots)?;

        // The creator may have asked for more slots than we sized the file for, and two
        // processes racing to create can interleave their `set_len` calls such that the
        // file ends up shorter than the winning header claims. Either way the header is
        // authoritative, so the mapping has to be made to cover it.
        let required = Self::file_len(effective);
        if mmap.len() < required {
            file.set_len(required as u64)?;
            mmap = unsafe { MmapMut::map_mut(&file)? };
            if mmap.len() < required {
                return Err(Error::BadFormat);
            }
        }

        Ok(ShmCache {
            mmap,
            num_slots: effective,
        })
    }

    fn file_len(num_slots: usize) -> usize {
        size_of::<Header>() + num_slots * size_of::<Slot>()
    }

    /// Claim the header, or wait for whoever claimed it first, and report the slot
    /// count that actually applies.
    ///
    /// A new file is zero-filled, so `magic == 0` marks it uninitialised. Exactly one
    /// process wins the compare-exchange and fills the header in; everyone else waits
    /// for `ready` and then adopts the winner's slot count rather than its own.
    fn init_header(mmap: &MmapMut, num_slots: usize) -> Result<usize, Error> {
        let header = unsafe { &*(mmap.as_ptr() as *const Header) };

        match header
            .magic
            .compare_exchange(0, MAGIC, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                // The OS already zeroed the file, so the slots need no further work.
                header.version.store(FORMAT_VERSION, Ordering::Relaxed);
                header.num_slots.store(num_slots as u32, Ordering::Relaxed);
                header.ready.store(1, Ordering::Release);
                Ok(num_slots)
            }
            Err(MAGIC) => {
                // The winner only has three relaxed stores to do, but it can still be
                // preempted between them, so this waits on the clock rather than on an
                // iteration count.
                let mut backoff = Backoff::new();
                loop {
                    if header.ready.load(Ordering::Acquire) == 1 {
                        if header.version.load(Ordering::Relaxed) != FORMAT_VERSION {
                            return Err(Error::BadFormat);
                        }
                        let existing = header.num_slots.load(Ordering::Relaxed) as usize;
                        if existing == 0 {
                            return Err(Error::BadFormat);
                        }
                        return Ok(existing);
                    }
                    if !backoff.wait(WRITE_LOCK_TIMEOUT) {
                        return Err(Error::BadFormat);
                    }
                }
            }
            Err(_) => Err(Error::BadFormat),
        }
    }

    /// Number of slots this cache actually has, which is the count the creating
    /// process chose — not necessarily the one passed to [`ShmCache::open`].
    pub fn capacity(&self) -> usize {
        self.num_slots
    }

    fn slot(&self, index: usize) -> &Slot {
        debug_assert!(index < self.num_slots);
        let base = unsafe { self.mmap.as_ptr().add(size_of::<Header>()) } as *const Slot;
        unsafe { &*base.add(index) }
    }

    fn slot_index(&self, key: &[u8]) -> usize {
        (fnv1a(key) as usize) % self.num_slots
    }

    /// Store `value` under `key`, replacing whatever occupied the slot.
    ///
    /// Returns [`Error::SlotStalled`] if the slot is held by a writer that never
    /// finished, which only happens if that process died mid-write.
    pub fn write(&self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        if key.len() > MAX_KEY_LEN {
            return Err(Error::KeyTooLong { len: key.len() });
        }
        if value.len() > MAX_VAL_LEN {
            return Err(Error::ValueTooLong { len: value.len() });
        }

        let slot = self.slot(self.slot_index(key));

        // Take the slot by moving seq from even to odd. Acquire on success keeps the
        // payload writes below from being hoisted above the lock.
        let mut backoff = Backoff::new();
        let locked = loop {
            let seq = slot.seq.load(Ordering::Relaxed);
            if seq & 1 == 0
                && slot
                    .seq
                    .compare_exchange_weak(seq, seq + 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                break seq;
            }
            if !backoff.wait(WRITE_LOCK_TIMEOUT) {
                return Err(Error::SlotStalled);
            }
        };

        // Raw pointers throughout: another process may be reading these bytes right
        // now, so forming a `&mut` to them would assert an exclusivity we do not have.
        let slot_mut = slot as *const Slot as *mut Slot;
        unsafe {
            (&raw mut (*slot_mut).key_len).write_volatile(key.len() as u32);
            (&raw mut (*slot_mut).val_len).write_volatile(value.len() as u32);
            std::ptr::copy_nonoverlapping(
                key.as_ptr(),
                (&raw mut (*slot_mut).key).cast::<u8>(),
                key.len(),
            );
            std::ptr::copy_nonoverlapping(
                value.as_ptr(),
                (&raw mut (*slot_mut).val).cast::<u8>(),
                value.len(),
            );
        }

        // Release publishes every write above to any reader that sees this seq.
        slot.seq.store(locked + 2, Ordering::Release);
        Ok(())
    }

    /// Look up `key`, copying the value out of the mapping.
    ///
    /// The copy is deliberate. It is what lets us re-check `seq` afterwards and know
    /// the bytes we returned were a single consistent snapshot — handing back a slice
    /// into the mapping would let another process rewrite it under the caller.
    ///
    /// Returns `None` for a miss, for a slot holding a different key (a collision),
    /// and for a slot stuck locked by a dead writer.
    pub fn read(&self, key: &[u8]) -> Option<Bytes> {
        if key.len() > MAX_KEY_LEN {
            return None;
        }

        let slot = self.slot(self.slot_index(key));
        let mut backoff = Backoff::new();

        loop {
            let before = slot.seq.load(Ordering::Acquire);
            if before & 1 != 0 {
                if !backoff.wait(READ_LOCK_TIMEOUT) {
                    return None;
                }
                continue;
            }

            // Everything below may be reading a half-written slot, so nothing is
            // trusted until the seq re-check at the bottom confirms otherwise.
            let slot_ptr = slot as *const Slot;
            let key_len = unsafe { (&raw const (*slot_ptr).key_len).read_volatile() } as usize;
            let val_len = unsafe { (&raw const (*slot_ptr).val_len).read_volatile() } as usize;

            // A torn read can produce nonsense lengths. Bounds-check before forming
            // any slice, or a torn read becomes an out-of-bounds read.
            if key_len > MAX_KEY_LEN || val_len > MAX_VAL_LEN {
                if !backoff.wait(READ_LOCK_TIMEOUT) {
                    return None;
                }
                continue;
            }

            if key_len != key.len() {
                // Either a genuine miss or a tear. Only conclude "miss" if the slot
                // was stable across the whole sample.
                fence(Ordering::Acquire);
                if slot.seq.load(Ordering::Acquire) == before {
                    return None;
                }
                if !backoff.wait(READ_LOCK_TIMEOUT) {
                    return None;
                }
                continue;
            }

            let mut key_buf = [0u8; MAX_KEY_LEN];
            let mut val_buf = [0u8; MAX_VAL_LEN];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&raw const (*slot_ptr).key).cast::<u8>(),
                    key_buf.as_mut_ptr(),
                    key_len,
                );
                std::ptr::copy_nonoverlapping(
                    (&raw const (*slot_ptr).val).cast::<u8>(),
                    val_buf.as_mut_ptr(),
                    val_len,
                );
            }

            // Keep the copies above from sinking below the re-read.
            fence(Ordering::Acquire);
            if slot.seq.load(Ordering::Acquire) != before {
                // Overwritten mid-copy, so these bytes are a mix of two values.
                if !backoff.wait(READ_LOCK_TIMEOUT) {
                    return None;
                }
                continue;
            }

            if key_buf[..key_len] != *key {
                return None;
            }
            return Some(Bytes::copy_from_slice(&val_buf[..val_len]));
        }
    }
}

/// FNV-1a.
///
/// This must be deterministic across processes. `RandomState` is seeded per process,
/// so using it here would have each process hash the same key to a different slot and
/// silently see an empty cache.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Self-checking values shared by the in-process test and the two-process test
/// helper binary (`src/bin/shm_worker.rs`). Bins cannot import a crate's test module,
/// so this lives in the library proper rather than under `#[cfg(test)]`.
#[doc(hidden)]
pub mod selftest {
    use super::fnv1a;

    /// Every value carries enough information to prove it was not assembled from two
    /// different writes: a counter, a checksum over the rest, and a payload derived
    /// from the counter. A torn read fails [`check_value`] instead of going unnoticed.
    pub const VALUE_LEN: usize = 128;

    pub fn make_value(counter: u64) -> Vec<u8> {
        let mut v = vec![0u8; VALUE_LEN];
        v[0..8].copy_from_slice(&counter.to_le_bytes());
        for (i, b) in v[16..].iter_mut().enumerate() {
            *b = (counter as u8).wrapping_add(i as u8);
        }
        let checksum = fnv1a(&digest_input(&v));
        v[8..16].copy_from_slice(&checksum.to_le_bytes());
        v
    }

    /// Returns the counter if the value is intact, `None` if it is torn.
    pub fn check_value(v: &[u8]) -> Option<u64> {
        if v.len() != VALUE_LEN {
            return None;
        }
        let counter = u64::from_le_bytes(v[0..8].try_into().ok()?);
        let checksum = u64::from_le_bytes(v[8..16].try_into().ok()?);
        if fnv1a(&digest_input(v)) != checksum {
            return None;
        }
        Some(counter)
    }

    /// Everything except the checksum field itself.
    fn digest_input(v: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(VALUE_LEN - 8);
        out.extend_from_slice(&v[0..8]);
        out.extend_from_slice(&v[16..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::selftest::{check_value, make_value};
    use super::*;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("minicache-{tag}-{}-{nanos}.shm", std::process::id()))
    }

    struct TempFile(std::path::PathBuf);
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn write_then_read() {
        let path = TempFile(temp_path("basic"));
        let cache = ShmCache::open(&path.0, 64).unwrap();

        cache.write(b"hello", b"world").unwrap();
        assert_eq!(cache.read(b"hello"), Some(Bytes::from_static(b"world")));
        assert_eq!(cache.read(b"missing"), None);
    }

    #[test]
    fn overwrite_wins() {
        let path = TempFile(temp_path("overwrite"));
        let cache = ShmCache::open(&path.0, 64).unwrap();

        cache.write(b"k", b"first").unwrap();
        cache.write(b"k", b"second").unwrap();
        assert_eq!(cache.read(b"k"), Some(Bytes::from_static(b"second")));
    }

    #[test]
    fn shorter_value_does_not_leave_a_tail() {
        // val_len shrinks but the old bytes are still in the slot; the read must be
        // bounded by val_len, not by whatever is left over.
        let path = TempFile(temp_path("shrink"));
        let cache = ShmCache::open(&path.0, 64).unwrap();

        cache.write(b"k", b"aaaaaaaaaaaaaaaa").unwrap();
        cache.write(b"k", b"bb").unwrap();
        assert_eq!(cache.read(b"k"), Some(Bytes::from_static(b"bb")));
    }

    #[test]
    fn empty_value_roundtrips() {
        let path = TempFile(temp_path("empty"));
        let cache = ShmCache::open(&path.0, 64).unwrap();

        cache.write(b"k", b"").unwrap();
        assert_eq!(cache.read(b"k"), Some(Bytes::new()));
    }

    #[test]
    fn rejects_oversized_key_and_value() {
        let path = TempFile(temp_path("oversize"));
        let cache = ShmCache::open(&path.0, 64).unwrap();

        let big_key = vec![b'k'; MAX_KEY_LEN + 1];
        let big_val = vec![b'v'; MAX_VAL_LEN + 1];
        assert!(matches!(
            cache.write(&big_key, b"v"),
            Err(Error::KeyTooLong { .. })
        ));
        assert!(matches!(
            cache.write(b"k", &big_val),
            Err(Error::ValueTooLong { .. })
        ));
    }

    #[test]
    fn max_sized_key_and_value_roundtrip() {
        let path = TempFile(temp_path("maxsize"));
        let cache = ShmCache::open(&path.0, 64).unwrap();

        let key = vec![b'k'; MAX_KEY_LEN];
        let val = vec![b'v'; MAX_VAL_LEN];
        cache.write(&key, &val).unwrap();
        assert_eq!(cache.read(&key).as_deref(), Some(&val[..]));
    }

    #[test]
    fn second_handle_sees_the_same_data() {
        // Same-process stand-in for the cross-process test: a second mapping of the
        // same file must observe the first handle's writes.
        let path = TempFile(temp_path("shared"));
        let a = ShmCache::open(&path.0, 64).unwrap();
        a.write(b"shared", b"value").unwrap();

        let b = ShmCache::open(&path.0, 64).unwrap();
        assert_eq!(b.read(b"shared"), Some(Bytes::from_static(b"value")));

        b.write(b"shared", b"updated").unwrap();
        assert_eq!(a.read(b"shared"), Some(Bytes::from_static(b"updated")));
    }

    #[test]
    fn existing_slot_count_wins_over_the_callers() {
        // Opening with a different slot count must not re-modulus the existing cache:
        // the two handles would hash the same key to different slots and silently stop
        // sharing, which looks like a cache that just never hits.
        let path = TempFile(temp_path("mismatch"));

        let creator = ShmCache::open(&path.0, 256).unwrap();
        creator.write(b"k", b"v").unwrap();

        for requested in [1, 64, 4096] {
            let joiner = ShmCache::open(&path.0, requested).unwrap();
            assert_eq!(
                joiner.capacity(),
                256,
                "opening with {requested} slots should adopt the creator's 256"
            );
            assert_eq!(joiner.read(b"k"), Some(Bytes::from_static(b"v")));

            joiner.write(b"k2", b"v2").unwrap();
            assert_eq!(creator.read(b"k2"), Some(Bytes::from_static(b"v2")));
        }
    }

    #[test]
    fn reopening_smaller_does_not_truncate() {
        // `set_len` shrinks, so a careless open would cut the file down and take live
        // slots with it.
        let path = TempFile(temp_path("truncate"));

        let big = ShmCache::open(&path.0, 2048).unwrap();
        // A key that lands in a high slot, which a truncated file would not contain.
        let key = b"tail-key";
        big.write(key, b"v").unwrap();
        let len_before = std::fs::metadata(&path.0).unwrap().len();

        let small = ShmCache::open(&path.0, 8).unwrap();
        assert_eq!(small.capacity(), 2048);
        assert_eq!(
            std::fs::metadata(&path.0).unwrap().len(),
            len_before,
            "file must not shrink"
        );
        assert_eq!(small.read(key), Some(Bytes::from_static(b"v")));
    }

    #[test]
    fn data_survives_reopen() {
        let path = TempFile(temp_path("persist"));
        {
            let cache = ShmCache::open(&path.0, 64).unwrap();
            cache.write(b"k", b"v").unwrap();
        }
        let cache = ShmCache::open(&path.0, 64).unwrap();
        assert_eq!(cache.read(b"k"), Some(Bytes::from_static(b"v")));
    }

    #[test]
    fn colliding_key_reads_as_a_miss() {
        // One slot, so every key collides. The occupant must not be returned for a
        // different key.
        let path = TempFile(temp_path("collide"));
        let cache = ShmCache::open(&path.0, 1).unwrap();

        cache.write(b"first", b"one").unwrap();
        assert_eq!(cache.read(b"second"), None);

        cache.write(b"second", b"two").unwrap();
        assert_eq!(cache.read(b"second"), Some(Bytes::from_static(b"two")));
        assert_eq!(cache.read(b"first"), None);
    }

    #[test]
    fn threads_never_observe_a_torn_value() {
        // The single-process warm-up for tests/shm_ipc.rs. Every value is
        // self-checking, so a torn read fails the checksum rather than going unnoticed.
        use std::sync::Arc;

        let path = TempFile(temp_path("threads"));
        let cache = Arc::new(ShmCache::open(&path.0, 16).unwrap());
        let writer_cache = Arc::clone(&cache);

        let writer = std::thread::spawn(move || {
            for i in 0..20_000u64 {
                let val = make_value(i);
                writer_cache.write(b"hot", &val).unwrap();
            }
        });

        let mut seen = 0usize;
        for _ in 0..200_000 {
            if let Some(v) = cache.read(b"hot") {
                assert!(check_value(&v).is_some(), "torn value observed: {v:?}");
                seen += 1;
            }
        }
        writer.join().unwrap();
        assert!(seen > 0, "reader never observed the key");
    }

}
