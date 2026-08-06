//! Two separate OS processes hammering the same key in one shared-memory cache.
//!
//! Threads would not prove anything here — they share an address space, so a
//! thread-based test passes even if the design accidentally depends on process-local
//! state. These are real `fork`/`exec`ed processes that only share the mmap.
//!
//! The property under test is that a reader never observes a half-written value.
//! Every value is self-checking (see `minicache::selftest`), so a torn read
//! fails its checksum instead of slipping through as a plausible-looking byte string.

use minicache::ShmCache;
use std::process::{Child, Command};

/// Cargo exports this for every `[[bin]]`, so the test always runs the binary from
/// the same build profile rather than guessing a path under `target/`.
const WORKER: &str = env!("CARGO_BIN_EXE_shm_worker");

const ITERATIONS: u64 = 200_000;

/// A shared-memory object outlives every process that used it, so each test needs a
/// name nothing else will pick and must unlink it afterwards or it survives until
/// reboot. Unlinking also runs up front, in case an earlier crashed run left one behind.
struct TempShm(String);

impl TempShm {
    fn new(tag: &str) -> Self {
        let name = format!("mc-i{tag}-{:x}", std::process::id());
        let _ = ShmCache::unlink(&name);
        TempShm(name)
    }

    fn name(&self) -> &str {
        &self.0
    }
}

impl Drop for TempShm {
    fn drop(&mut self) {
        let _ = ShmCache::unlink(&self.0);
    }
}

fn spawn(name: &str, role: &str, iterations: u64) -> Child {
    Command::new(WORKER)
        .arg(name)
        .arg(role)
        .arg(iterations.to_string())
        .spawn()
        .unwrap_or_else(|e| panic!("could not spawn {role} worker at {WORKER}: {e}"))
}

#[test]
fn two_processes_share_one_key_without_torn_reads() {
    let shm = TempShm::new("torn");

    // Reader first, so it is already polling when the writer's first value lands.
    // It tolerates the key being absent, so losing the race is harmless.
    let mut reader = spawn(shm.name(), "reader", ITERATIONS);
    let mut writer = spawn(shm.name(), "writer", ITERATIONS);

    let writer_status = writer.wait().expect("writer never exited");
    let reader_status = reader.wait().expect("reader never exited");

    assert!(
        writer_status.success(),
        "writer process failed: {writer_status}"
    );
    // The reader fails if it saw a torn value, never saw the key (which would mean
    // the mapping is not actually shared), or timed out before the writer's last value.
    assert!(
        reader_status.success(),
        "reader process failed: {reader_status}"
    );
}

#[test]
fn reader_starting_late_still_sees_the_final_value() {
    // The writer finishes before the reader is even spawned, so the only way the
    // value can be found is through the shared-memory object itself.
    let shm = TempShm::new("late");

    let writer_status = spawn(shm.name(), "writer", 1_000)
        .wait()
        .expect("writer never exited");
    assert!(
        writer_status.success(),
        "writer process failed: {writer_status}"
    );

    let reader_status = spawn(shm.name(), "reader", 1_000)
        .wait()
        .expect("reader never exited");
    assert!(
        reader_status.success(),
        "reader process failed: {reader_status}"
    );
}

#[test]
fn concurrent_writers_race_to_create_and_contend() {
    // Two things at once. All three processes race to initialise a cache that does not
    // exist yet: exactly one should win the header compare-exchange, and the losers
    // must wait rather than reinitialising and wiping the winner's slots.
    //
    // They then all hammer the same key, so they contend on one slot's write lock.
    // A write must not fail just because another live writer holds the slot — only a
    // writer that actually died should ever produce `SlotStalled`.
    let shm = TempShm::new("race");

    let mut workers: Vec<Child> = (0..3)
        .map(|_| spawn(shm.name(), "writer", 5_000))
        .collect();

    for (i, w) in workers.iter_mut().enumerate() {
        let status = w.wait().expect("worker never exited");
        assert!(status.success(), "writer {i} failed: {status}");
    }

    // Both wrote the same key, so whichever finished last wins — but the value must
    // be intact and readable by a third process.
    let reader_status = spawn(shm.name(), "reader", 5_000)
        .wait()
        .expect("reader never exited");
    assert!(
        reader_status.success(),
        "reader process failed: {reader_status}"
    );
}
