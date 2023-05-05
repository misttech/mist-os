// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    fuchsia_merkle::MerkleTree,
    fuchsia_zircon as zx,
    rand::{
        distributions::{Distribution, WeightedIndex},
        seq::SliceRandom,
        Rng, SeedableRng,
    },
    rand_xorshift::XorShiftRng,
    std::{
        fs::OpenOptions,
        io::Write,
        iter::{Iterator, StepBy},
        ops::Range,
        os::unix::io::AsRawFd,
        path::{Path, PathBuf},
        vec::Vec,
    },
    storage_benchmarks::{
        trace_duration, Benchmark, Filesystem, OperationDuration, OperationTimer,
    },
};

const RNG_SEED: u64 = 0xda782a0c3ce1819a;

macro_rules! page_in_benchmark {
    ($benchmark:ident, $data_gen_fn:ident, $page_iter_gen_fn:ident) => {
        #[derive(Clone)]
        pub struct $benchmark {
            blob_size: usize,
        }

        impl $benchmark {
            pub fn new(blob_size: usize) -> Self {
                Self { blob_size }
            }
        }

        #[async_trait]
        impl Benchmark for $benchmark {
            async fn run(&self, fs: &mut dyn Filesystem) -> Vec<OperationDuration> {
                trace_duration!(
                    "benchmark",
                    stringify!($benchmark),
                    "blob_size" => self.blob_size as u64
                );
                let mut rng = XorShiftRng::seed_from_u64(RNG_SEED);
                let data = $data_gen_fn(self.blob_size, &mut rng);
                let page_iter = $page_iter_gen_fn(self.blob_size, &mut rng);
                page_in_blob_benchmark(fs, data, page_iter).await
            }

            fn name(&self) -> String {
                stringify!($benchmark).to_string()
            }
        }
    }
}

page_in_benchmark!(
    PageInBlobSequentialUncompressed,
    create_incompressible_data,
    sequential_page_iter
);
page_in_benchmark!(PageInBlobSequentialCompressed, create_compressible_data, sequential_page_iter);
page_in_benchmark!(PageInBlobRandomCompressed, create_compressible_data, random_page_iter);

struct MappedBlob {
    addr: *mut libc::c_void,
    size: libc::size_t,
}

impl MappedBlob {
    fn new(blob_path: &Path) -> Self {
        let file = OpenOptions::new().read(true).open(blob_path).unwrap();
        let size = file.metadata().unwrap().len() as libc::size_t;
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        assert!(addr != libc::MAP_FAILED, "Failed to mmap blob: {:?}", errno_error());
        Self { addr, size }
    }

    fn data(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.addr as *const u8, self.size) }
    }
}

impl Drop for MappedBlob {
    fn drop(&mut self) {
        let ret = unsafe { libc::munmap(self.addr, self.size) };
        assert!(ret == 0, "Failed to munmap blob: {:?}", errno_error());
    }
}

/// Write the blob to the filesystem and return the path to the bob.
fn write_blob(blob_dir: &Path, data: &[u8]) -> PathBuf {
    let merkle = MerkleTree::from_reader(data).unwrap().root();
    let blob_path = blob_dir.join(merkle.to_string());

    let mut file = OpenOptions::new().write(true).create_new(true).open(&blob_path).unwrap();
    file.set_len(data.len() as u64).unwrap();
    file.write_all(data).unwrap();

    blob_path
}

/// Returns completely random data that shouldn't be compressible.
fn create_incompressible_data(size: usize, rng: &mut XorShiftRng) -> Vec<u8> {
    let mut data = vec![0; size];
    rng.fill(data.as_mut_slice());
    data
}

/// Creates runs of the same byte between 2 and 8 bytes long. This should compress to about 40% of
/// the original size which is typical for large executable blobs.
fn create_compressible_data(size: usize, rng: &mut XorShiftRng) -> Vec<u8> {
    const RUN_RANGE: Range<usize> = 2..8;
    let mut data = vec![0u8; size];
    let mut rest = data.as_mut_slice();
    while !rest.is_empty() {
        let chunk = if rest.len() < RUN_RANGE.end { rest.len() } else { rng.gen_range(RUN_RANGE) };
        let value: u8 = rng.gen();
        let (l, r) = rest.split_at_mut(chunk);
        rest = r;
        l.fill(value);
    }
    data
}

/// Returns an iterator to the index of the first byte of every page in sequential order.
fn sequential_page_iter(blob_size: usize, _rng: &mut XorShiftRng) -> impl Iterator<Item = usize> {
    let page_size = zx::system_get_page_size() as usize;
    (0..blob_size).step_by(page_size)
}

/// Returns an iterator to the index of the first byte of every page. The order of the pages tries
/// to mimic how pages are accessed if the blob was an executable.
fn random_page_iter(blob_size: usize, rng: &mut XorShiftRng) -> impl Iterator<Item = usize> {
    // Executables tend to both randomly jump between pages and go on long runs of sequentially
    // accessing pages.
    const RUN_LENGTHS: [usize; 6] = [1, 3, 15, 40, 60, 80];
    const WEIGHTS: [usize; 6] = [25, 15, 40, 10, 6, 4];
    let distribution = WeightedIndex::new(WEIGHTS).unwrap();
    let page_size = zx::system_get_page_size() as usize;
    let total_pages = (0..blob_size).step_by(page_size).len();

    // Split the pages up into runs.
    let mut taken_pages = 0;
    let mut page_runs: Vec<StepBy<Range<usize>>> = Vec::new();
    while taken_pages < total_pages {
        let index = distribution.sample(rng);
        let run_length = std::cmp::min(RUN_LENGTHS[index], total_pages - taken_pages);
        let start = taken_pages * page_size;
        let end = (taken_pages + run_length) * page_size;
        taken_pages += run_length;
        page_runs.push((start..end).step_by(page_size));
    }

    page_runs.shuffle(rng);
    page_runs.into_iter().flatten()
}

async fn page_in_blob_benchmark(
    fs: &mut dyn Filesystem,
    data: Vec<u8>,
    page_iter: impl Iterator<Item = usize>,
) -> Vec<OperationDuration> {
    let blob_path = {
        trace_duration!("benchmark", "write-blob");
        write_blob(fs.benchmark_dir(), &data)
    };
    std::mem::drop(data);

    fs.clear_cache().await;

    let mapped_blob = MappedBlob::new(&blob_path);
    let data = mapped_blob.data();
    let mut durations = Vec::new();
    let mut total = 0;
    for i in page_iter {
        trace_duration!("benchmark", "page_in", "offset" => i as u64);
        let timer = OperationTimer::start();
        let value = data[i];
        durations.push(timer.stop());
        total += value as u64;
    }
    assert!(total > 0);
    durations
}

fn errno_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

#[cfg(test)]
mod tests {
    use {super::*, storage_benchmarks::testing::TestFilesystem};
    const PAGE_COUNT: usize = 32;

    fn page_size() -> usize {
        zx::system_get_page_size() as usize
    }

    async fn check_benchmark(benchmark: impl Benchmark, op_count: usize, clear_cache_count: u64) {
        let mut test_fs = Box::new(TestFilesystem::new());
        let results = benchmark.run(test_fs.as_mut()).await;

        assert_eq!(results.len(), op_count);
        assert_eq!(test_fs.clear_cache_count().await, clear_cache_count);
        test_fs.shutdown().await;
    }

    #[fuchsia::test]
    async fn page_in_blob_sequential_compressed_test() {
        check_benchmark(
            PageInBlobSequentialCompressed::new(PAGE_COUNT * page_size()),
            PAGE_COUNT,
            /*clear_cache_count=*/ 1,
        )
        .await;
    }

    #[fuchsia::test]
    async fn page_in_blob_sequential_uncompressed_test() {
        check_benchmark(
            PageInBlobSequentialUncompressed::new(PAGE_COUNT * page_size()),
            PAGE_COUNT,
            /*clear_cache_count=*/ 1,
        )
        .await;
    }

    #[fuchsia::test]
    async fn page_in_blob_random_compressed_test() {
        check_benchmark(
            PageInBlobRandomCompressed::new(PAGE_COUNT * page_size()),
            PAGE_COUNT,
            /*clear_cache_count=*/ 1,
        )
        .await;
    }

    #[fuchsia::test]
    fn sequential_page_iter_test() {
        let mut rng = XorShiftRng::seed_from_u64(RNG_SEED);
        assert_eq!(sequential_page_iter(0, &mut rng).max(), None);
        assert_eq!(sequential_page_iter(1, &mut rng).max(), Some(0));
        assert_eq!(sequential_page_iter(page_size() - 1, &mut rng).max(), Some(0));
        assert_eq!(sequential_page_iter(page_size(), &mut rng).max(), Some(0));
        assert_eq!(sequential_page_iter(page_size() + 1, &mut rng).max(), Some(page_size()));

        let offsets: Vec<usize> = sequential_page_iter(page_size() * 4, &mut rng).collect();
        assert_eq!(&offsets, &[0, page_size(), page_size() * 2, page_size() * 3]);
    }

    #[fuchsia::test]
    fn random_page_iter_test() {
        let mut rng = XorShiftRng::seed_from_u64(RNG_SEED);
        assert_eq!(random_page_iter(0, &mut rng).max(), None);
        assert_eq!(random_page_iter(1, &mut rng).max(), Some(0));
        assert_eq!(random_page_iter(page_size() - 1, &mut rng).max(), Some(0));
        assert_eq!(random_page_iter(page_size(), &mut rng).max(), Some(0));
        assert_eq!(random_page_iter(page_size() + 1, &mut rng).max(), Some(page_size()));
        assert_eq!(random_page_iter(page_size() * 5, &mut rng).count(), 5);

        let mut offsets: Vec<usize> = random_page_iter(page_size() * 500, &mut rng).collect();

        // Make sure that the offsets aren't sorted.
        let mut is_sorted = true;
        for i in 1..offsets.len() {
            if offsets[i - 1] >= offsets[i] {
                is_sorted = false;
                break;
            }
        }
        assert!(!is_sorted);

        // Sort the offsets and compare to the sequential offsets to make sure they're all present.
        let expected_sorted_offsets: Vec<usize> =
            sequential_page_iter(page_size() * 500, &mut rng).collect();
        offsets.sort();
        assert_eq!(offsets, expected_sorted_offsets);
    }
}
