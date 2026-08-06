#![cfg(feature = "_selftest")]

use shmcache::ShmCache;
use std::process::{Child, Command};

const WORKER: &str = env!("CARGO_BIN_EXE_shm_worker");

const ITERATIONS: u64 = 200_000;

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

    let mut reader = spawn(shm.name(), "reader", ITERATIONS);
    let mut writer = spawn(shm.name(), "writer", ITERATIONS);

    let writer_status = writer.wait().expect("writer never exited");
    let reader_status = reader.wait().expect("reader never exited");

    assert!(
        writer_status.success(),
        "writer process failed: {writer_status}"
    );
    assert!(
        reader_status.success(),
        "reader process failed: {reader_status}"
    );
}

#[test]
fn reader_starting_late_still_sees_the_final_value() {
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
    let shm = TempShm::new("race");

    let mut workers: Vec<Child> = (0..3).map(|_| spawn(shm.name(), "writer", 5_000)).collect();

    for (i, w) in workers.iter_mut().enumerate() {
        let status = w.wait().expect("worker never exited");
        assert!(status.success(), "writer {i} failed: {status}");
    }

    let reader_status = spawn(shm.name(), "reader", 5_000)
        .wait()
        .expect("reader never exited");
    assert!(
        reader_status.success(),
        "reader process failed: {reader_status}"
    );
}
