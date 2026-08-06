//! Test helper for `tests/shm_ipc.rs`.
//!
//! Two of these run as separate OS processes against the same mapped file, one
//! writing and one reading the same key. Kept as a real binary rather than threads
//! because threads share an address space, which is exactly the thing being tested.
//!
//! Usage: `shm_worker <path> <writer|reader> <iterations>`
//!
//! Exits non-zero on any violated assertion so failures surface through `cargo test`.

use minicache::shm::ShmCache;
use minicache::shm::selftest::{check_value, make_value};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const KEY: &[u8] = b"hot_key";
const NUM_SLOTS: usize = 16;
const READER_TIMEOUT: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: {} <path> <writer|reader> <iterations>", args[0]);
        return ExitCode::FAILURE;
    }

    let path = &args[1];
    let role = args[2].as_str();
    let iterations: u64 = match args[3].parse() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("bad iteration count: {e}");
            return ExitCode::FAILURE;
        }
    };

    let cache = match ShmCache::open(path, NUM_SLOTS) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[{role}] could not open {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    match role {
        "writer" => writer(&cache, iterations),
        "reader" => reader(&cache, iterations),
        other => {
            eprintln!("unknown role: {other}");
            ExitCode::FAILURE
        }
    }
}

fn writer(cache: &ShmCache, iterations: u64) -> ExitCode {
    for i in 0..iterations {
        if let Err(e) = cache.write(KEY, &make_value(i)) {
            eprintln!("[writer] write {i} failed: {e}");
            return ExitCode::FAILURE;
        }
    }
    eprintln!("[writer] wrote {iterations} values");
    ExitCode::SUCCESS
}

fn reader(cache: &ShmCache, iterations: u64) -> ExitCode {
    let final_counter = iterations - 1;
    let deadline = Instant::now() + READER_TIMEOUT;

    let mut hits = 0u64;
    let mut distinct = std::collections::HashSet::new();
    let mut highest = 0u64;

    // The writer runs flat out, so most values are missed. That is fine: what is
    // being tested is that no value is ever observed half-written, and that the
    // writer's final value crosses the process boundary.
    //
    // The clock is only checked periodically — reading it every pass would dominate
    // the loop and starve the reader of samples.
    let mut spins = 0u32;
    loop {
        spins += 1;
        if spins.is_multiple_of(4096) && Instant::now() >= deadline {
            break;
        }

        let Some(value) = cache.read(KEY) else {
            continue;
        };
        hits += 1;

        let Some(counter) = check_value(&value) else {
            eprintln!(
                "[reader] TORN VALUE after {hits} hits: {:?}",
                &value[..value.len().min(32)]
            );
            return ExitCode::FAILURE;
        };

        if counter > final_counter {
            eprintln!("[reader] counter {counter} exceeds the writer's last value");
            return ExitCode::FAILURE;
        }

        distinct.insert(counter);
        highest = highest.max(counter);
        if counter == final_counter {
            break;
        }
    }

    if hits == 0 {
        eprintln!("[reader] never saw the key at all — the mapping is not shared");
        return ExitCode::FAILURE;
    }
    if highest != final_counter {
        eprintln!(
            "[reader] timed out at counter {highest}, expected to reach {final_counter} \
             ({hits} hits, {} distinct values)",
            distinct.len()
        );
        return ExitCode::FAILURE;
    }

    eprintln!(
        "[reader] {hits} hits, {} distinct values, reached {highest}, zero torn reads",
        distinct.len()
    );
    ExitCode::SUCCESS
}
