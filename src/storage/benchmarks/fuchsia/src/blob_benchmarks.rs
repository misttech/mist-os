// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    blob_writer::BlobWriter,
    fidl_fuchsia_fxfs::WriteBlobProxy,
    fidl_fuchsia_io as fio,
    fuchsia_component::client::connect_to_protocol_at_dir_svc,
    fuchsia_merkle::MerkleTree,
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

macro_rules! write_blobs_benchmark {
    ($benchmark:ident, $write_blobs_benchmark:ident) => {
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
                let mut data = Vec::new();
                for _n in 1..=5 {
                    data.push(create_compressible_data(self.blob_size, &mut rng));
                }
                $write_blobs_benchmark(fs, data).await
            }

            fn name(&self) -> String {
                format!("{}/{}", stringify!($benchmark), self.blob_size)
            }
        }
    };
}

macro_rules! write_realistic_blobs_benchmark {
    ($benchmark:ident, $write_blobs_benchmark:ident) => {
        #[derive(Clone)]
        pub struct $benchmark {}

        impl $benchmark {
            pub fn new() -> Self {
                Self {}
            }
        }

        #[async_trait]
        impl Benchmark for $benchmark {
            async fn run(&self, fs: &mut dyn Filesystem) -> Vec<OperationDuration> {
                trace_duration!("benchmark", stringify!($benchmark));
                let mut rng = XorShiftRng::seed_from_u64(RNG_SEED);
                let mut data = Vec::new();
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
                for size in sizes {
                    data.push(create_compressible_data(size, &mut rng));
                }
                $write_blobs_benchmark(fs, data).await
            }

            fn name(&self) -> String {
                stringify!($benchmark).to_string()
            }
        }
    };
}

page_in_benchmark!(
    PageInBlobSequentialUncompressed,
    create_incompressible_data,
    sequential_page_iter
);
page_in_benchmark!(PageInBlobSequentialCompressed, create_compressible_data, sequential_page_iter);
page_in_benchmark!(PageInBlobRandomCompressed, create_compressible_data, random_page_iter);

write_blobs_benchmark!(WriteBlobWithFidl, write_blobs_with_fidl_benchmark);
write_blobs_benchmark!(WriteBlobWithBlobWriter, write_blobs_with_blob_writer_benchmark);
write_realistic_blobs_benchmark!(
    WriteRealisticBlobsWithFidl,
    write_realistic_blobs_with_fidl_benchmark
);
write_realistic_blobs_benchmark!(
    WriteRealisticBlobsWithBlobWriter,
    write_realistic_blobs_with_blob_writer_benchmark
);

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

/// Creates, truncates, and writes a new blob using fuchsia.io.
async fn write_blob_with_fidl(blob_root: &fio::DirectoryProxy, data: &[u8], merkle: &str) {
    let blob = fuchsia_fs::directory::open_file(
        blob_root,
        merkle,
        fuchsia_fs::OpenFlags::CREATE | fuchsia_fs::OpenFlags::RIGHT_WRITABLE,
    )
    .await
    .unwrap();
    let blob_size = data.len();
    let () = blob.resize(blob_size as u64).await.unwrap().unwrap();
    let mut written = 0;
    while written != blob_size {
        // Don't try to write more than MAX_TRANSFER_SIZE bytes at a time.
        let bytes_to_write = std::cmp::min(fio::MAX_TRANSFER_SIZE, (blob_size - written) as u64);
        let bytes_written =
            blob.write(&data[written..written + bytes_to_write as usize]).await.unwrap().unwrap();
        assert_eq!(bytes_written, bytes_to_write);
        written += bytes_written as usize;
    }
}

/// Creates, truncates, and writes a new blob using a BlobWriter.
async fn write_blob_with_blob_writer(blob_proxy: &WriteBlobProxy, data: &[u8], merkle: &[u8; 32]) {
    let writer_client_end = blob_proxy
        .create(merkle, false)
        .await
        .expect("transport error on WriteBlob.Create")
        .expect("failed to create blob");
    let writer = writer_client_end.into_proxy().unwrap();
    let mut blob_writer =
        BlobWriter::create(writer, data.len() as u64).await.expect("failed to create BlobWriter");
    blob_writer.write(&data).await.unwrap();
}

async fn write_blobs_with_fidl_benchmark(
    fs: &mut dyn Filesystem,
    blobs: Vec<Vec<u8>>,
) -> Vec<OperationDuration> {
    let mut durations = Vec::new();
    let blob_root = fuchsia_fs::directory::open_in_namespace(
        fs.benchmark_dir().to_str().unwrap(),
        fuchsia_fs::OpenFlags::RIGHT_WRITABLE | fuchsia_fs::OpenFlags::RIGHT_READABLE,
    )
    .unwrap();
    for blob in blobs {
        let merkle = MerkleTree::from_reader(blob.as_slice()).unwrap().root();
        let total_duration = OperationTimer::start();
        write_blob_with_fidl(&blob_root, &blob, &merkle.to_string()).await;
        durations.push(total_duration.stop());
    }
    durations
}

async fn write_blobs_with_blob_writer_benchmark(
    fs: &mut dyn Filesystem,
    blobs: Vec<Vec<u8>>,
) -> Vec<OperationDuration> {
    let mut durations = Vec::new();
    let blob_proxy =
        connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::WriteBlobMarker>(fs.exposed_dir())
            .expect("failed to connect to the WriteBlob service");
    for blob in blobs {
        let merkle = MerkleTree::from_reader(blob.as_slice()).unwrap().root();
        let timer = OperationTimer::start();
        write_blob_with_blob_writer(&blob_proxy, &blob, &merkle.into()).await;
        durations.push(timer.stop());
    }
    durations
}

async fn write_realistic_blobs_with_fidl_benchmark(
    fs: &mut dyn Filesystem,
    blobs: Vec<Vec<u8>>,
) -> Vec<OperationDuration> {
    let mut futures = Vec::new();
    let blob_root = fuchsia_fs::directory::open_in_namespace(
        fs.benchmark_dir().to_str().unwrap(),
        fuchsia_fs::OpenFlags::RIGHT_WRITABLE | fuchsia_fs::OpenFlags::RIGHT_READABLE,
    )
    .unwrap();
    for blob in blobs {
        let merkle = MerkleTree::from_reader(blob.as_slice()).unwrap().root();
        let blob_root_clone = std::clone::Clone::clone(&blob_root);
        let blob_future = async move {
            write_blob_with_fidl(&blob_root_clone, &blob, &merkle.to_string()).await;
        };
        futures.push(blob_future);
    }
    let fut = stream::iter(futures).for_each_concurrent(2, |blob_future| async move {
        blob_future.await;
    });
    let timer = OperationTimer::start();
    fut.await;
    vec![timer.stop()]
}

async fn write_realistic_blobs_with_blob_writer_benchmark(
    fs: &mut dyn Filesystem,
    blobs: Vec<Vec<u8>>,
) -> Vec<OperationDuration> {
    let mut futures = Vec::new();
    let blob_proxy =
        connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::WriteBlobMarker>(fs.exposed_dir())
            .expect("failed to connect to the WriteBlob service");
    for blob in blobs {
        let merkle = MerkleTree::from_reader(blob.as_slice()).unwrap().root();
        let blob_proxy_clone = blob_proxy.clone();
        let blob_future = async move {
            write_blob_with_blob_writer(&blob_proxy_clone, &blob, &merkle.into()).await;
        };
        futures.push(blob_future);
    }
    let fut = stream::iter(futures).for_each_concurrent(2, |blob_future| async move {
        blob_future.await;
    });
    let timer = OperationTimer::start();
    fut.await;
    vec![timer.stop()]
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{block_devices::RamdiskFactory, filesystems::Fxblob},
        storage_benchmarks::{testing::TestFilesystem, FilesystemConfig},
    };
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

    async fn check_new_write_blobs_benchmark(benchmark: impl Benchmark, op_count: usize) {
        let ramdisk_factory = RamdiskFactory::new(page_size() as u64, 35000).await;
        let mut test_fs = Fxblob::new().start_filesystem(&ramdisk_factory).await;
        let results = benchmark.run(test_fs.as_mut()).await;

        assert_eq!(results.len(), op_count);
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
    async fn write_small_blob_with_fidl_test() {
        check_benchmark(
            WriteBlobWithFidl::new(PAGE_COUNT * page_size()),
            5,
            /*clear_cache_count=*/ 0,
        )
        .await;
    }

    #[fuchsia::test]
    async fn write_small_blob_with_blob_writer_test() {
        check_new_write_blobs_benchmark(WriteBlobWithBlobWriter::new(PAGE_COUNT * page_size()), 5)
            .await;
    }

    #[fuchsia::test]
    async fn write_large_blob_with_fidl_test() {
        check_benchmark(
            WriteBlobWithFidl::new(PAGE_COUNT * page_size() * 25),
            5,
            /*clear_cache_count=*/ 0,
        )
        .await;
    }

    #[fuchsia::test]
    async fn write_large_blob_with_blob_writer_test() {
        check_new_write_blobs_benchmark(
            WriteBlobWithBlobWriter::new(PAGE_COUNT * page_size() * 25),
            5,
        )
        .await;
    }

    #[fuchsia::test]
    async fn write_realistic_blobs_with_fidl_test() {
        check_benchmark(WriteRealisticBlobsWithFidl::new(), 1, /*clear_cache_count=*/ 0).await;
    }

    #[fuchsia::test]
    async fn write_realistic_blobs_with_blob_writer_test() {
        check_new_write_blobs_benchmark(WriteRealisticBlobsWithBlobWriter::new(), 1).await;
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
