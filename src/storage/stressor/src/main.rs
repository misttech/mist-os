// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    rand::{distributions::WeightedIndex, prelude::*, seq::SliceRandom},
    std::{
        fs::File,
        io::ErrorKind,
        os::unix::fs::FileExt,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Mutex, RwLock,
        },
    },
};

#[derive(Default)]
struct Stressor {
    name_counter: AtomicU64,
    all_files: RwLock<Vec<Arc<FileState>>>,
    open_files: RwLock<Vec<Arc<std::fs::File>>>,
    op_stats: Mutex<[u64; NUM_OPS]>,
}

struct FileState {
    path: String,
}

const OPEN_FILE: usize = 0;
const CLOSE_FILE: usize = 1;
const CREATE_FILE: usize = 2;
const DELETE_FILE: usize = 3;
const READ: usize = 4;
const WRITE: usize = 5;
const TRUNCATE: usize = 6;

const NUM_OPS: usize = 7;

impl Stressor {
    fn worker(&self) {
        let mut rng = rand::thread_rng();
        let mut buf = Vec::new();
        loop {
            let op = WeightedIndex::new(self.get_weights()).unwrap().sample(&mut rng);
            match op {
                OPEN_FILE => {
                    let Some(file) = self.all_files.read().unwrap().choose(&mut rng).cloned()
                    else {
                        continue;
                    };
                    match File::options().read(true).write(true).open(&file.path) {
                        Ok(f) => self.open_files.write().unwrap().push(Arc::new(f)),
                        Err(e) => {
                            // Allow not found in case we raced with deleting a file.
                            assert_eq!(e.kind(), ErrorKind::NotFound);
                        }
                    }
                }
                CLOSE_FILE => {
                    let _file = {
                        let mut open_files = self.open_files.write().unwrap();
                        let num = open_files.len();
                        if num == 0 {
                            continue;
                        }
                        open_files.remove(rng.gen_range(0..num))
                    };
                }
                CREATE_FILE => {
                    let path =
                        format!("/data/{}", self.name_counter.fetch_add(1, Ordering::Relaxed));
                    let file =
                        File::options().create(true).read(true).write(true).open(&path).unwrap();
                    self.all_files.write().unwrap().push(Arc::new(FileState { path }));
                    self.open_files.write().unwrap().push(Arc::new(file));
                }
                DELETE_FILE => {
                    let file = {
                        let mut all_files = self.all_files.write().unwrap();
                        if all_files.is_empty() {
                            continue;
                        }
                        let num = all_files.len();
                        all_files.remove(rng.gen_range(0..num))
                    };
                    std::fs::remove_file(&file.path).unwrap();
                }
                READ => {
                    let Some(file) = self.open_files.read().unwrap().choose(&mut rng).cloned()
                    else {
                        continue;
                    };
                    let read_len = (-rng.gen::<f64>().ln() * 10000.0) as usize;
                    let read_offset = rng.gen_range(0..100_000);
                    buf.resize(read_len, 1);
                    file.read_at(&mut buf, read_offset).unwrap();
                }
                WRITE => {
                    let Some(file) = self.open_files.read().unwrap().choose(&mut rng).cloned()
                    else {
                        continue;
                    };
                    let write_len = (-rng.gen::<f64>().ln() * 10000.0) as usize;
                    let write_offset = rng.gen_range(0..100_000);
                    buf.resize(write_len, 1);
                    file.write_at(&buf, write_offset).unwrap();
                }
                TRUNCATE => {
                    let Some(file) = self.open_files.read().unwrap().choose(&mut rng).cloned()
                    else {
                        continue;
                    };
                    file.set_len(rng.gen_range(0..100_000)).unwrap();
                }
                _ => unreachable!(),
            }
            self.op_stats.lock().unwrap()[op] += 1;
        }
    }

    fn get_weights(&self) -> [f64; NUM_OPS] {
        let all_file_count = self.all_files.read().unwrap().len() as f64;
        let open_file_count = self.open_files.read().unwrap().len();
        let if_open = |x| if open_file_count > 0 { x } else { 0.0 };
        let open_file_count = open_file_count as f64;
        [
            /* OPEN_FILE: */ 1.0 / (open_file_count + 1.0) * all_file_count,
            /* CLOSE_FILE: */ open_file_count * 0.1,
            /* CREATE_FILE: */ 2.0 / (all_file_count + 1.0),
            /* DELETE_FILE: */ all_file_count * 0.005,
            /* READ: */ if_open(1000.0),
            /* WRITE: */ if_open(1000.0),
            /* TRUNCATE: */ if_open(1000.0),
        ]
    }
}

#[fuchsia::main]
fn main() {
    // Find any existing files.
    let mut files = Vec::new();
    let mut max_counter = 0;
    for entry in std::fs::read_dir("/data").unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if let Some(counter) =
            path.strip_prefix("/data/").ok().and_then(|p| p.to_str()).and_then(|s| s.parse().ok())
        {
            if counter > max_counter {
                max_counter = counter;
            }
        }
        files.push(Arc::new(FileState { path: path.to_str().unwrap().to_string() }));
    }

    tracing::info!("Running stressor, found {} files, counter: {}", files.len(), max_counter);

    let stressor = Arc::new(Stressor {
        name_counter: AtomicU64::new(max_counter + 1),
        all_files: RwLock::new(files),
        ..Stressor::default()
    });

    for _ in 0..8 {
        let stressor = stressor.clone();
        std::thread::spawn(move || stressor.worker());
    }

    loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        tracing::info!(
            "{} files, {} open, weights: {:?}, counts: {:?}",
            stressor.all_files.read().unwrap().len(),
            stressor.open_files.read().unwrap().len(),
            stressor.get_weights(),
            stressor.op_stats.lock().unwrap()
        );
    }
}
