// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Starts the 'aggressive' stressor.
//! This variant performs open-read-close, open-append-close, and unlink-open-append-close
//! operations. Note that 'truncate' is not exercised.
//! This version also silently ignores errors such as out of space.

use {
    fidl_fuchsia_io as fio,
    fuchsia_async::Time,
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
/// Unaligned random reads and append-only writes up to 128kB each in length.
/// Write errors causes the file to be truncated to zero bytes.
#[derive(Default)]
pub struct Stressor {
    /// Used to round-robin across NUM_FILES in order.
    file_counter: AtomicU64,
    /// Tracks the number of bytes we have allocated.
    bytes_stored: AtomicU64,
    /// The target number of bytes we want to allocate before deleting files.
    target_bytes: u64,
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
    /// Creates a new aggressive stressor that will try to fill the disk until
    /// `target_free_bytes` of free space remains.
    pub fn new(target_free_bytes: u64) -> Arc<Self> {
        // Work out how many bytes to target.
        let dir = fio::DirectorySynchronousProxy::new(
            fuchsia_async::Channel::from_channel(
                fdio::transfer_fd(std::fs::File::open("/data").unwrap()).unwrap().into(),
            )
            .into(),
        );
        let info = dir.query_filesystem(Time::INFINITE.into()).unwrap().1.unwrap();
        let bytes_stored = info.used_bytes;
        let target_bytes = info.total_bytes - target_free_bytes;
        tracing::info!(
            "Aggressive stressor mode targetting {} of {} bytes on device.",
            target_bytes,
            info.total_bytes
        );

        Arc::new(Stressor {
            file_counter: AtomicU64::new(0),
            bytes_stored: AtomicU64::new(bytes_stored),
            target_bytes,
            ..Stressor::default()
        })
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
            tracing::info!(
                "bytes_stored: {}, target_bytes {}, counts: {:?}",
                self.bytes_stored.load(Ordering::Relaxed),
                self.target_bytes,
                self.op_stats.lock().unwrap()
            );
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
                        let read_len = rng.gen_range(0..128 * 1024u64);
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
                            if self.bytes_stored.load(Ordering::Relaxed) >= self.target_bytes {
                                self.bytes_stored.fetch_sub(file_len, Ordering::Relaxed);
                                let _ = f.set_len(0).unwrap();
                            } else {
                                buf.resize(rng.gen_range(0..128 * 1024), 1);
                                match f.write_at(&buf, file_len) {
                                    Ok(bytes) => {
                                        self.bytes_stored
                                            .fetch_add(bytes as u64, Ordering::Relaxed);
                                    }
                                    Err(_) => {
                                        // When a write fails due to space, we truncate.
                                        // In this way we fill up the disk then start fragmenting it.
                                        // Until #![feature(io_error_more)] is stable, we have
                                        // a catch-all here.
                                        self.bytes_stored.fetch_sub(file_len, Ordering::Relaxed);
                                        let _ = f.set_len(0).unwrap();
                                    }
                                };
                            }
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
