// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::filesystems::{BlobFilesystem, DeliveryBlob},
    async_trait::async_trait,
    delivery_blob::CompressionMode,
    fuchsia_zircon as zx,
    futures::stream::{self, StreamExt},
    rand::{
        distributions::{Distribution, WeightedIndex},
        seq::SliceRandom,
        Rng, SeedableRng,
    },
    rand_xorshift::XorShiftRng,
    std::{
        fs::OpenOptions,
        iter::{Iterator, StepBy},
        ops::Range,
        os::unix::io::AsRawFd,
        path::Path,
        vec::Vec,
    },
    storage_benchmarks::{trace_duration, Benchmark, OperationDuration, OperationTimer},
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
        impl<T:BlobFilesystem> Benchmark<T> for $benchmark {
            async fn run(&self, fs: &mut T) -> Vec<OperationDuration> {
                trace_duration!(
                    "benchmark",
                    stringify!($benchmark),
                    "blob_size" => self.blob_size as u64
                );
                let mut rng = XorShiftRng::seed_from_u64(RNG_SEED);
                let blob = $data_gen_fn(self.blob_size, &mut rng);
                let page_iter = $page_iter_gen_fn(self.blob_size, &mut rng);
                page_in_blob_benchmark(fs, blob, page_iter).await
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

#[derive(Clone)]
pub struct WriteBlob {
    blob_size: usize,
}

impl WriteBlob {
    pub fn new(blob_size: usize) -> Self {
        Self { blob_size }
    }
}

#[async_trait]
impl<T: BlobFilesystem> Benchmark<T> for WriteBlob {
    async fn run(&self, fs: &mut T) -> Vec<OperationDuration> {
        trace_duration!(
            "benchmark",
            "WriteBlob",
            "blob_size" => self.blob_size as u64
        );
        const SAMPLES: usize = 5;
        let mut rng = XorShiftRng::seed_from_u64(RNG_SEED);
        let mut durations = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let blob = create_compressible_data(self.blob_size, &mut rng);
            let total_duration = OperationTimer::start();
            fs.write_blob(&blob).await;
            durations.push(total_duration.stop());
        }
        durations
    }

    fn name(&self) -> String {
        format!("WriteBlob/{}", self.blob_size)
    }
}

#[derive(Clone)]
pub struct WriteRealisticBlobs {}

impl WriteRealisticBlobs {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl<T: BlobFilesystem> Benchmark<T> for WriteRealisticBlobs {
    async fn run(&self, fs: &mut T) -> Vec<OperationDuration> {
        trace_duration!("benchmark", "WriteRealisticBlobs");
        // Only write 2 blobs at once to match pkg-cache.
        const CONCURRENT_WRITE_COUNT: usize = 2;

        let mut rng = XorShiftRng::seed_from_u64(RNG_SEED);
        let sizes = vec![
            67 * 1024 * 1024,
            33 * 1024 * 1024,
            2 * 1024 * 1024,
            1024 * 1024,
            131072,
            65536,
            65536,
            32768,
            16384,
            16384,
            4096,
            4096,
            4096,
            4096,
            4096,
            4096,
        ];

        let mut futures = Vec::with_capacity(sizes.len());
        for size in sizes {
            let blob = create_compressible_data(size, &mut rng);
            let fs: &T = fs;
            futures.push(async move {
                fs.write_blob(&blob).await;
            });
        }

        let fut = stream::iter(futures).for_each_concurrent(
            CONCURRENT_WRITE_COUNT,
            |blob_future| async move {
                blob_future.await;
            },
        );

        let timer = OperationTimer::start();
        fut.await;
        vec![timer.stop()]
    }

    fn name(&self) -> String {
        "WriteRealisticBlobs".to_string()
    }
}

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

/// Returns completely random data that shouldn't be compressible.
fn create_incompressible_data(size: usize, rng: &mut XorShiftRng) -> DeliveryBlob {
    let mut data = vec![0; size];
    rng.fill(data.as_mut_slice());
    DeliveryBlob::new(data, CompressionMode::Never)
}

/// Creates runs of the same byte between 2 and 8 bytes long. This should compress to about 40% of
/// the original size which is typical for large executable blobs.
fn create_compressible_data(size: usize, rng: &mut XorShiftRng) -> DeliveryBlob {
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
    DeliveryBlob::new(data, CompressionMode::Always)
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
    // Only access 60% of the pages. Not all pages of an executable are typically accessed near the
    // start of a process. Accessing every page would favour filesystems with overly aggressive
    // read-ahead when in practice some of the read-ahead pages won't be used.
    let pages_to_read = if total_pages < 5 { total_pages } else { total_pages / 5 * 3 };

    // Split the pages up into runs.
    let mut taken_pages = 0;
    let mut page_runs: Vec<StepBy<Range<usize>>> = Vec::new();
    while taken_pages < pages_to_read {
        let index = distribution.sample(rng);
        let run_length = std::cmp::min(RUN_LENGTHS[index], pages_to_read - taken_pages);
        let start = taken_pages * page_size;
        let end = (taken_pages + run_length) * page_size;
        taken_pages += run_length;
        page_runs.push((start..end).step_by(page_size));
    }

    page_runs.shuffle(rng);
    page_runs.into_iter().flatten()
}

async fn page_in_blob_benchmark(
    fs: &mut impl BlobFilesystem,
    blob: DeliveryBlob,
    page_iter: impl Iterator<Item = usize>,
) -> Vec<OperationDuration> {
    let blob_path = fs.benchmark_dir().join(blob.name.to_string());
    {
        trace_duration!("benchmark", "write-blob");
        fs.write_blob(&blob).await;
    };

    fs.clear_cache().await;

    let mapped_blob = MappedBlob::new(&blob_path);
    let data = mapped_blob.data();
    let mut durations = Vec::new();
    for i in page_iter {
        trace_duration!("benchmark", "page_in", "offset" => i as u64);
        let timer = OperationTimer::start();
        std::hint::black_box(data[i]);
        durations.push(timer.stop());
    }
    durations
}

fn errno_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            block_devices::RamdiskFactory,
            filesystems::{Blobfs, Fxblob},
        },
        storage_benchmarks::FilesystemConfig,
        test_util::assert_lt,
    };
    const PAGE_COUNT: usize = 32;

    fn page_size() -> usize {
        zx::system_get_page_size() as usize
    }

    async fn check_benchmark<T, U, V>(benchmark: T, filesystem_config: V, op_count: usize)
    where
        T: Benchmark<U>,
        U: BlobFilesystem,
        V: FilesystemConfig<Filesystem = U>,
    {
        let blocks = 200 * 1024 * 1024 / page_size() as u64; // 200MiB
        let ramdisk_factory = RamdiskFactory::new(page_size() as u64, blocks).await;
        let mut filesystem = filesystem_config.start_filesystem(&ramdisk_factory).await;
        let results = benchmark.run(&mut filesystem).await;
        assert_eq!(results.len(), op_count);
        filesystem.shutdown().await;
    }

    #[fuchsia::test]
    async fn page_in_blob_sequential_compressed_blobfs_test() {
        check_benchmark(
            PageInBlobSequentialCompressed::new(PAGE_COUNT * page_size()),
            Blobfs,
            PAGE_COUNT,
        )
        .await;
    }

    #[fuchsia::test]
    async fn page_in_blob_sequential_compressed_fxblob_test() {
        check_benchmark(
            PageInBlobSequentialCompressed::new(PAGE_COUNT * page_size()),
            Fxblob,
            PAGE_COUNT,
        )
        .await;
    }

    #[fuchsia::test]
    async fn page_in_blob_sequential_uncompressed_test() {
        check_benchmark(
            PageInBlobSequentialUncompressed::new(PAGE_COUNT * page_size()),
            Fxblob,
            PAGE_COUNT,
        )
        .await;
    }

    #[fuchsia::test]
    async fn page_in_blob_random_compressed_test() {
        check_benchmark(
            PageInBlobRandomCompressed::new(PAGE_COUNT * page_size()),
            Fxblob,
            PAGE_COUNT / 5 * 3,
        )
        .await;
    }

    #[fuchsia::test]
    async fn write_blob_blobfs_test() {
        check_benchmark(WriteBlob::new(PAGE_COUNT * page_size()), Blobfs, 5).await;
    }

    #[fuchsia::test]
    async fn write_blob_fxblob_test() {
        check_benchmark(WriteBlob::new(PAGE_COUNT * page_size()), Fxblob, 5).await;
    }

    #[fuchsia::test]
    async fn write_realistic_blobs_blobfs_test() {
        check_benchmark(WriteRealisticBlobs::new(), Blobfs, 1).await;
    }

    #[fuchsia::test]
    async fn write_realistic_blobs_fxblob_test() {
        check_benchmark(WriteRealisticBlobs::new(), Fxblob, 1).await;
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
        assert_eq!(random_page_iter(page_size() * 4, &mut rng).count(), 4);
        assert_eq!(random_page_iter(page_size() * 5, &mut rng).count(), 3);
        assert_eq!(random_page_iter(page_size() * 9, &mut rng).count(), 3);
        assert_eq!(random_page_iter(page_size() * 10, &mut rng).count(), 6);

        let blob_size = page_size() * 500;
        let mut offsets: Vec<usize> = random_page_iter(blob_size, &mut rng).collect();

        // Make sure that the offsets aren't sorted.
        let mut is_sorted = true;
        for i in 1..offsets.len() {
            if offsets[i - 1] >= offsets[i] {
                is_sorted = false;
                break;
            }
        }
        assert!(!is_sorted);

        offsets.sort();
        // Make sure that there are no duplicates.
        for i in 1..offsets.len() {
            assert_ne!(offsets[i - 1], offsets[i]);
        }
        // Make sure that the largest page offset is part of the blob.
        assert_lt!(offsets.last().unwrap(), &blob_size);
    }
}
