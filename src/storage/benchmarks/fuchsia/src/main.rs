// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        blob_benchmarks::{
            PageInBlobRandomCompressed, PageInBlobSequentialCompressed,
            PageInBlobSequentialUncompressed, WriteBlob, WriteRealisticBlobs,
        },
        block_devices::{FvmVolumeFactory, RamdiskFactory},
        filesystems::{Blobfs, F2fs, Fxblob, Fxfs, Memfs, Minfs},
    },
    regex::{Regex, RegexSetBuilder},
    std::{fs::File, path::PathBuf, vec::Vec},
    storage_benchmarks::{
        add_benchmarks,
        directory_benchmarks::{
            DirectoryTreeStructure, GitStatus, OpenDeeplyNestedFile, OpenFile, StatPath,
            WalkDirectoryTreeCold, WalkDirectoryTreeWarm,
        },
        io_benchmarks::{
            ReadRandomCold, ReadRandomWarm, ReadSequentialCold, ReadSequentialWarm,
            WriteRandomCold, WriteRandomWarm, WriteSequentialCold, WriteSequentialFsyncCold,
            WriteSequentialFsyncWarm, WriteSequentialWarm,
        },
        BenchmarkSet, Filesystem as _, FilesystemConfig,
    },
};

mod blob_benchmarks;
mod block_devices;
mod filesystems;

/// Fuchsia Filesystem Benchmarks
#[derive(argh::FromArgs)]
struct Args {
    /// path to write the fuchsiaperf formatted benchmark results to.
    #[argh(option)]
    output_fuchsiaperf: Option<PathBuf>,

    /// outputs a summary of the benchmark results in csv format.
    #[argh(switch)]
    output_csv: bool,

    /// regex to specify a subset of benchmarks to run. Multiple regex can be provided and
    /// benchmarks matching any of them will be run. The benchmark names are formatted as
    /// "<benchmark>/<filesystem>". All benchmarks are run if no filter is provided.
    #[argh(option)]
    filter: Vec<Regex>,

    /// registers a trace provider and adds a trace duration with the benchmarks name around each
    /// benchmark.
    #[argh(switch)]
    enable_tracing: bool,

    /// starts each filesystem to force the filesystem components and the benchmark component to be
    /// loaded into blobfs. Does not run any benchmarks.
    ///
    /// When trying to collect a trace immediately after modifying a filesystem or a benchmark, the
    /// start of the trace will be polluted with downloading the new blobs, writing the blobs to
    /// blobfs, and then paging the blobs back in. Running the benchmark component with this flag
    /// once before running it again with tracing enabled will remove most of the component loading
    /// from the start of the trace.
    #[argh(switch)]
    load_blobs_for_tracing: bool,
}

fn add_io_benchmarks(benchmark_set: &mut BenchmarkSet) {
    const OP_SIZE: usize = 8 * 1024;
    const OP_COUNT: usize = 1024;
    add_benchmarks!(
        benchmark_set,
        [
            ReadSequentialCold::new(OP_SIZE, OP_COUNT),
            ReadSequentialWarm::new(OP_SIZE, OP_COUNT),
            ReadRandomCold::new(OP_SIZE, OP_COUNT),
            ReadRandomWarm::new(OP_SIZE, OP_COUNT),
            WriteSequentialCold::new(OP_SIZE, OP_COUNT),
            WriteSequentialWarm::new(OP_SIZE, OP_COUNT),
            WriteRandomCold::new(OP_SIZE, OP_COUNT),
            WriteRandomWarm::new(OP_SIZE, OP_COUNT),
            WriteSequentialFsyncCold::new(OP_SIZE, OP_COUNT),
            WriteSequentialFsyncWarm::new(OP_SIZE, OP_COUNT),
        ],
        [Fxfs, F2fs, Memfs, Minfs]
    );
}

fn add_directory_benchmarks(benchmark_set: &mut BenchmarkSet) {
    // Creates a total of 62 directories and 189 files.
    let dts = DirectoryTreeStructure {
        files_per_directory: 3,
        directories_per_directory: 2,
        max_depth: 5,
    };
    add_benchmarks!(
        benchmark_set,
        [
            StatPath::new(),
            OpenFile::new(),
            OpenDeeplyNestedFile::new(),
            WalkDirectoryTreeCold::new(dts, 20),
            WalkDirectoryTreeWarm::new(dts, 20),
            GitStatus::new(),
        ],
        [Fxfs, F2fs, Memfs, Minfs]
    );
}

fn add_blob_benchmarks(benchmark_set: &mut BenchmarkSet) {
    const SMALL_BLOB_SIZE: usize = 2 * 1024 * 1024; // 2 MiB
    const LARGE_BLOB_SIZE: usize = 25 * 1024 * 1024; // 25 MiB
    add_benchmarks!(
        benchmark_set,
        [
            PageInBlobSequentialUncompressed::new(SMALL_BLOB_SIZE),
            PageInBlobSequentialCompressed::new(SMALL_BLOB_SIZE),
            PageInBlobRandomCompressed::new(SMALL_BLOB_SIZE),
            WriteBlob::new(SMALL_BLOB_SIZE),
            WriteBlob::new(LARGE_BLOB_SIZE),
            WriteRealisticBlobs::new(),
        ],
        [Blobfs, Fxblob]
    );
}

async fn load_blobs_for_tracing() {
    let ramdisk_factory = RamdiskFactory::new(4096, 32 * 1024).await;
    Fxfs.start_filesystem(&ramdisk_factory).await.shutdown().await;
    F2fs.start_filesystem(&ramdisk_factory).await.shutdown().await;
    Memfs.start_filesystem(&ramdisk_factory).await.shutdown().await;
    Minfs.start_filesystem(&ramdisk_factory).await.shutdown().await;
    Blobfs.start_filesystem(&ramdisk_factory).await.shutdown().await;
    Fxblob.start_filesystem(&ramdisk_factory).await.shutdown().await;
}

#[fuchsia::main(logging_tags = ["storage_benchmarks"])]
async fn main() {
    let args: Args = argh::from_env();

    if args.load_blobs_for_tracing {
        load_blobs_for_tracing().await;
        return;
    }

    if args.enable_tracing {
        fuchsia_trace_provider::trace_provider_create_with_fdio();
    }

    let mut filter = RegexSetBuilder::new(args.filter.iter().map(|f| f.as_str()));
    filter.case_insensitive(true);
    let filter = filter.build().unwrap();

    let fvm_volume_factory = FvmVolumeFactory::new().await;
    if fvm_volume_factory.is_none() {
        tracing::warn!("Not running any tests -- neither FVM nor GPT could be found.");
        tracing::warn!("To run these test locally on an emulator, see the README.md.");
        return;
    }

    let mut benchmark_set = BenchmarkSet::new();
    add_io_benchmarks(&mut benchmark_set);
    add_directory_benchmarks(&mut benchmark_set);
    add_blob_benchmarks(&mut benchmark_set);
    let results = benchmark_set.run(fvm_volume_factory.as_ref().unwrap(), &filter).await;

    results.write_table(std::io::stdout());
    if args.output_csv {
        results.write_csv(std::io::stdout())
    }
    if let Some(path) = args.output_fuchsiaperf {
        let file = File::create(path).unwrap();
        results.write_fuchsia_perf_json(file);
    }
}
