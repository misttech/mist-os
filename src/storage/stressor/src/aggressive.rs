// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Starts the 'aggressive' stressor.
//! This variant performs open-read-close, open-append-close, and unlink-open-append-close
//! operations. Note that 'truncate' is not exercised.
//! This version also silently ignores errors such as out of space.

use {
    rand::{distributions::WeightedIndex, prelude::*},
    std::{
        fs::File,
        io::ErrorKind,
        os::unix::fs::FileExt,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Mutex,
        },
    },
};

/// Designed to 'exercise' the filesystem and thrash the dirent cache.
/// This cyclically works its way through 10000 files (the dirent cache holds 8000).
/// On each iteration it will either read, write or rewrite a file.
/// Unaligned random reads up to 256kB -- more than read-ahead boundaries at 128kiB.
/// Non-sparse writes up to 256kB -- creates inner fragmentation without holes.
/// Rewrite is an unlink and a write operation -- this creates fragmentation when disk fills.
#[derive(Default)]
pub struct Stressor {
    file_counter: AtomicU64,
    op_stats: Mutex<[u64; NUM_OPS]>,
}

#[derive(Eq, PartialEq)]
struct FileState {
    path: String,
}

const READ: usize = 0;
const WRITE: usize = 1;
const NUM_OPS: usize = 2;

/// The number of files to cycle through.
/// This must be larger than DIRENT_CACHE_LIMIT to induce cache thrashing.
/// See src/storage/fxfs/platform/src/fuchsia/volume.rs.
const NUM_FILES: u64 = 10000;

// We favour writes because half of those writes will end up being truncates.
// The expected long-term mix will therefore be around 1:1 reads/writes.
const WEIGHTS: [f64; NUM_OPS] = [/* READ: */ 1.0, /* WRITE: */ 2.0];

impl Stressor {
    pub fn new() -> Arc<Self> {
        Arc::new(Stressor { file_counter: AtomicU64::new(0), ..Stressor::default() })
    }

    /// Starts the stressor. Loops forever.
    pub fn run(self: &Arc<Self>, num_threads: usize) {
        for _ in 0..num_threads {
            let this = self.clone();
            std::thread::spawn(move || this.worker());
        }
        // Sleep forever. No real stats to produce here as weights are constant.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
            tracing::info!("counts: {:?}", self.op_stats.lock().unwrap());
        }
    }

    /// Worker thread function.
    fn worker(&self) {
        let mut rng = rand::thread_rng();
        let mut buf = Vec::new();
        loop {
            let op = WeightedIndex::new(WEIGHTS).unwrap().sample(&mut rng);
            let path =
                format!("/data/{}", self.file_counter.fetch_add(1, Ordering::Relaxed) % NUM_FILES);
            match op {
                READ => match File::options().read(true).write(false).open(&path) {
                    Ok(f) => {
                        let file_len = f.metadata().unwrap().len();
                        let read_len = rng.gen_range(0..160 * 1024u64);
                        let end = file_len.saturating_sub(read_len);
                        let offset = rng.gen_range(0..end + 1);
                        buf.resize(read_len as usize, 0);
                        f.read_at(&mut buf, offset).unwrap();
                    }
                    Err(e) => match e.kind() {
                        ErrorKind::NotFound => {}
                        error => {
                            tracing::warn!("Got an open error on READ: {error:?}");
                        }
                    },
                },
                WRITE => {
                    match File::options().create(true).read(true).write(true).open(&path) {
                        Ok(f) => {
                            let file_len = f.metadata().unwrap().len();
                            let offset = rng.gen_range(0..file_len + 1);
                            buf.resize(rng.gen_range(0..160 * 1024), 1);
                            match f.write_at(&buf, offset) {
                                Ok(_) => {}
                                Err(_) => {
                                    // When a write fails due to space, we truncate.
                                    // In this way we fill up the disk then start fragmenting it.
                                    // Until #![feature(io_error_more)] is stable, we have
                                    // a catch-all here.
                                    let _ = f.set_len(0).unwrap();
                                }
                            };
                        }
                        // Unfortunately ErrorKind::StorageFull is unstable so
                        // we rely on a catch-all here and assume write errors are
                        // due to disk being full. This happens often so we don't log it.
                        Err(_) => {}
                    }
                }
                _ => unreachable!(),
            }
            self.op_stats.lock().unwrap()[op] += 1;
        }
    }
}
