#![doc = include_str!("../README.md")]

#[cfg(not(unix))]
compile_error!("shmcache requires a Unix platform: it is built on POSIX shared memory");

use bytes::Bytes;
use memmap2::MmapMut;
use std::ffi::CString;
use std::fs::File;
use std::hint;
use std::io;
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::time::{Duration, Instant};

const MAGIC: u64 = 0x4d49_4e49_4341_4348;
const FORMAT_VERSION: u32 = 1;

pub const MAX_APP_NAME_LEN: usize = 30;

pub const MAX_KEY_LEN: usize = 64;
pub const MAX_VAL_LEN: usize = 1024;

const SPIN_ATTEMPTS: usize = 64;

const WRITE_LOCK_TIMEOUT: Duration = Duration::from_millis(50);

const READ_LOCK_TIMEOUT: Duration = Duration::from_millis(5);

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

    fn wait(&mut self, timeout: Duration) -> bool {
        self.attempts += 1;
        if self.attempts <= SPIN_ATTEMPTS {
            hint::spin_loop();
            return true;
        }
        let deadline = *self
            .deadline
            .get_or_insert_with(|| Instant::now() + timeout);
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::yield_now();
        true
    }
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    KeyTooLong { len: usize },
    ValueTooLong { len: usize },
    BadFormat,
    InvalidAppName { reason: &'static str },
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
            Error::BadFormat => write!(f, "object is not a shmcache shared-memory cache"),
            Error::InvalidAppName { reason } => write!(f, "invalid app name: {reason}"),
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

#[repr(C, align(64))]
struct Header {
    magic: AtomicU64,
    version: AtomicU32,
    num_slots: AtomicU32,
    ready: AtomicU32,
    _pad: [u8; 44],
}

#[repr(C, align(64))]
struct Slot {
    seq: AtomicU64,
    key_len: u32,
    val_len: u32,
    key: [u8; MAX_KEY_LEN],
    val: [u8; MAX_VAL_LEN],
}

pub struct ShmCache {
    mmap: MmapMut,
    num_slots: usize,
}

unsafe impl Send for ShmCache {}
unsafe impl Sync for ShmCache {}

impl ShmCache {
    pub fn open(app_name: &str, num_slots: usize) -> Result<Self, Error> {
        assert!(num_slots > 0, "num_slots must be > 0");
        assert!(
            num_slots <= u32::MAX as usize,
            "num_slots must fit in a u32"
        );

        let name = shm_name(app_name)?;
        let (object, created) = open_shm_object(&name)?;

        if created {
            object.set_len(Self::file_len(num_slots) as u64)?;
        } else {
            let mut backoff = Backoff::new();
            while (object.metadata()?.len() as usize) < size_of::<Header>() {
                if !backoff.wait(WRITE_LOCK_TIMEOUT) {
                    return Err(Error::BadFormat);
                }
            }
        }

        let mmap = unsafe { MmapMut::map_mut(&object)? };
        if mmap.len() < size_of::<Header>() {
            return Err(Error::BadFormat);
        }

        let effective = Self::init_header(&mmap, num_slots)?;

        if mmap.len() < Self::file_len(effective) {
            return Err(Error::BadFormat);
        }

        Ok(ShmCache {
            mmap,
            num_slots: effective,
        })
    }

    pub fn unlink(app_name: &str) -> Result<(), Error> {
        let name = shm_name(app_name)?;
        if unsafe { libc::shm_unlink(name.as_ptr()) } != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ENOENT) {
                return Err(Error::Io(err));
            }
        }
        Ok(())
    }

    fn file_len(num_slots: usize) -> usize {
        size_of::<Header>() + num_slots * size_of::<Slot>()
    }

    fn init_header(mmap: &MmapMut, num_slots: usize) -> Result<usize, Error> {
        let header = unsafe { &*(mmap.as_ptr() as *const Header) };

        match header
            .magic
            .compare_exchange(0, MAGIC, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                header.version.store(FORMAT_VERSION, Ordering::Relaxed);
                header.num_slots.store(num_slots as u32, Ordering::Relaxed);
                header.ready.store(1, Ordering::Release);
                Ok(num_slots)
            }
            Err(MAGIC) => {
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

    pub fn write(&self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        if key.len() > MAX_KEY_LEN {
            return Err(Error::KeyTooLong { len: key.len() });
        }
        if value.len() > MAX_VAL_LEN {
            return Err(Error::ValueTooLong { len: value.len() });
        }

        let slot = self.slot(self.slot_index(key));

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

        slot.seq.store(locked + 2, Ordering::Release);
        Ok(())
    }

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

            let slot_ptr = slot as *const Slot;
            let key_len = unsafe { (&raw const (*slot_ptr).key_len).read_volatile() } as usize;
            let val_len = unsafe { (&raw const (*slot_ptr).val_len).read_volatile() } as usize;

            if key_len > MAX_KEY_LEN || val_len > MAX_VAL_LEN {
                if !backoff.wait(READ_LOCK_TIMEOUT) {
                    return None;
                }
                continue;
            }

            if key_len != key.len() {
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

            fence(Ordering::Acquire);
            if slot.seq.load(Ordering::Acquire) != before {
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

fn shm_name(app_name: &str) -> Result<CString, Error> {
    if app_name.is_empty() {
        return Err(Error::InvalidAppName {
            reason: "name is empty",
        });
    }
    if app_name.len() > MAX_APP_NAME_LEN {
        return Err(Error::InvalidAppName {
            reason: "name is longer than 30 bytes, which macOS rejects",
        });
    }
    if !app_name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(Error::InvalidAppName {
            reason: "name may only contain alphanumerics, '-', '_' and '.'",
        });
    }

    CString::new(format!("/{app_name}")).map_err(|_| Error::InvalidAppName {
        reason: "name contains an interior nul byte",
    })
}

fn open_shm_object(name: &CString) -> Result<(File, bool), Error> {
    const MODE: libc::mode_t = 0o600;

    let created = unsafe {
        libc::shm_open(
            name.as_ptr(),
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
            MODE as libc::c_uint,
        )
    };
    if created >= 0 {
        return Ok((unsafe { File::from_raw_fd(created) }, true));
    }

    let err = io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EEXIST) {
        return Err(Error::Io(err));
    }

    let joined = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR, MODE as libc::c_uint) };
    if joined < 0 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    Ok((unsafe { File::from_raw_fd(joined) }, false))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[doc(hidden)]
#[cfg(any(feature = "_selftest", test))]
pub mod selftest {
    use super::fnv1a;

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

    struct TempShm(String);

    impl TempShm {
        fn new() -> Self {
            use std::sync::atomic::AtomicU64;
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!("mc-t{:x}-{:x}", std::process::id(), n);
            let _ = ShmCache::unlink(&name);
            TempShm(name)
        }

        fn open(&self, num_slots: usize) -> ShmCache {
            ShmCache::open(&self.0, num_slots).unwrap()
        }
    }

    impl Drop for TempShm {
        fn drop(&mut self) {
            let _ = ShmCache::unlink(&self.0);
        }
    }

    #[test]
    fn write_then_read() {
        let shm = TempShm::new();
        let cache = shm.open(64);

        cache.write(b"hello", b"world").unwrap();
        assert_eq!(cache.read(b"hello"), Some(Bytes::from_static(b"world")));
        assert_eq!(cache.read(b"missing"), None);
    }

    #[test]
    fn overwrite_wins() {
        let shm = TempShm::new();
        let cache = shm.open(64);

        cache.write(b"k", b"first").unwrap();
        cache.write(b"k", b"second").unwrap();
        assert_eq!(cache.read(b"k"), Some(Bytes::from_static(b"second")));
    }

    #[test]
    fn shorter_value_does_not_leave_a_tail() {
        let shm = TempShm::new();
        let cache = shm.open(64);

        cache.write(b"k", b"aaaaaaaaaaaaaaaa").unwrap();
        cache.write(b"k", b"bb").unwrap();
        assert_eq!(cache.read(b"k"), Some(Bytes::from_static(b"bb")));
    }

    #[test]
    fn empty_value_roundtrips() {
        let shm = TempShm::new();
        let cache = shm.open(64);

        cache.write(b"k", b"").unwrap();
        assert_eq!(cache.read(b"k"), Some(Bytes::new()));
    }

    #[test]
    fn rejects_oversized_key_and_value() {
        let shm = TempShm::new();
        let cache = shm.open(64);

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
        let shm = TempShm::new();
        let cache = shm.open(64);

        let key = vec![b'k'; MAX_KEY_LEN];
        let val = vec![b'v'; MAX_VAL_LEN];
        cache.write(&key, &val).unwrap();
        assert_eq!(cache.read(&key).as_deref(), Some(&val[..]));
    }

    #[test]
    fn second_handle_sees_the_same_data() {
        let shm = TempShm::new();
        let a = shm.open(64);
        a.write(b"shared", b"value").unwrap();

        let b = shm.open(64);
        assert_eq!(b.read(b"shared"), Some(Bytes::from_static(b"value")));

        b.write(b"shared", b"updated").unwrap();
        assert_eq!(a.read(b"shared"), Some(Bytes::from_static(b"updated")));
    }

    #[test]
    fn existing_slot_count_wins_over_the_callers() {
        let shm = TempShm::new();

        let creator = shm.open(256);
        creator.write(b"k", b"v").unwrap();

        for requested in [1, 64, 4096] {
            let joiner = shm.open(requested);
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
    fn joining_smaller_keeps_every_slot_reachable() {
        let shm = TempShm::new();

        let big = shm.open(2048);
        let high_slot_keys: Vec<Vec<u8>> = (0..64u64)
            .map(|i| format!("key-{i}").into_bytes())
            .filter(|k| big.slot_index(k) >= 8)
            .collect();
        assert!(
            !high_slot_keys.is_empty(),
            "test needs keys beyond the joiner's requested slot count"
        );
        for k in &high_slot_keys {
            big.write(k, b"v").unwrap();
        }

        let small = shm.open(8);
        assert_eq!(small.capacity(), 2048);
        for k in &high_slot_keys {
            assert_eq!(
                small.read(k),
                Some(Bytes::from_static(b"v")),
                "key in slot {} unreachable from the joiner",
                big.slot_index(k)
            );
        }
    }

    #[test]
    fn rejects_unusable_app_names() {
        let too_long = "x".repeat(MAX_APP_NAME_LEN + 1);
        for (name, why) in [
            ("", "empty"),
            (too_long.as_str(), "longer than macOS accepts"),
            ("has/slash", "a slash makes it a different POSIX object"),
            ("has space", "space"),
            ("emoji\u{1f600}", "non-ascii"),
        ] {
            assert!(
                matches!(ShmCache::open(name, 8), Err(Error::InvalidAppName { .. })),
                "{why:?} should be rejected: {name:?}"
            );
        }

        let at_limit = "a".repeat(MAX_APP_NAME_LEN);
        let cache = ShmCache::open(&at_limit, 8);
        assert!(cache.is_ok(), "a name of exactly the limit should work");
        drop(cache);
        ShmCache::unlink(&at_limit).unwrap();
    }

    #[test]
    fn unlink_starts_the_next_open_fresh() {
        let shm = TempShm::new();
        {
            let cache = shm.open(64);
            cache.write(b"k", b"v").unwrap();
            assert_eq!(cache.read(b"k"), Some(Bytes::from_static(b"v")));
        }

        ShmCache::unlink(&shm.0).unwrap();

        let cache = shm.open(64);
        assert_eq!(cache.read(b"k"), None, "unlink should discard the contents");

        ShmCache::unlink(&shm.0).unwrap();
        ShmCache::unlink(&shm.0).unwrap();
    }

    #[test]
    fn nothing_is_written_to_disk() {
        let shm = TempShm::new();
        let cache = shm.open(64);
        cache.write(b"k", b"v").unwrap();

        for dir in [std::env::temp_dir(), std::env::current_dir().unwrap()] {
            let stray = dir.join(&shm.0);
            assert!(
                !stray.exists(),
                "cache should not appear on disk, found {}",
                stray.display()
            );
        }
    }

    #[test]
    fn data_survives_reopen() {
        let shm = TempShm::new();
        {
            let cache = shm.open(64);
            cache.write(b"k", b"v").unwrap();
        }
        let cache = shm.open(64);
        assert_eq!(cache.read(b"k"), Some(Bytes::from_static(b"v")));
    }

    #[test]
    fn colliding_key_reads_as_a_miss() {
        let shm = TempShm::new();
        let cache = shm.open(1);

        cache.write(b"first", b"one").unwrap();
        assert_eq!(cache.read(b"second"), None);

        cache.write(b"second", b"two").unwrap();
        assert_eq!(cache.read(b"second"), Some(Bytes::from_static(b"two")));
        assert_eq!(cache.read(b"first"), None);
    }

    #[test]
    fn threads_never_observe_a_torn_value() {
        use std::sync::Arc;

        let shm = TempShm::new();
        let cache = Arc::new(shm.open(16));
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
