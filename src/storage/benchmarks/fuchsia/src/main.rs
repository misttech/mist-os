// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        blob_benchmarks::{
            PageInBlobRandomCompressed, PageInBlobSequentialCompressed,
            PageInBlobSequentialUncompressed,
        },
        block_devices::{FvmVolumeFactory, RamdiskFactory},
        filesystems::{Blobfs, F2fs, Fxblob, Fxfs, Memfs, Minfs},
    },
    regex::{Regex, RegexSetBuilder},
    std::{fs::File, path::PathBuf, sync::Arc, vec::Vec},
    storage_benchmarks::{
        directory_benchmarks::{
            DirectoryTreeStructure, GitStatus, OpenDeeplyNestedFile, OpenFile, StatPath,
            WalkDirectoryTreeCold, WalkDirectoryTreeWarm,
        },
        io_benchmarks::{
            ReadRandomCold, ReadRandomWarm, ReadSequentialCold, ReadSequentialWarm,
            WriteRandomCold, WriteRandomWarm, WriteSequentialCold, WriteSequentialFsyncCold,
            WriteSequentialFsyncWarm, WriteSequentialWarm,
        },
        BenchmarkSet, FilesystemConfig,
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

    /// runs the blob benchmarks instead of the regular benchmarks.
    #[argh(switch)]
    blob_benchmarks: bool,
}

fn build_benchmark_set() -> BenchmarkSet {
    let filesystems: Vec<Arc<dyn FilesystemConfig>> = vec![
        Arc::new(Fxfs::new()),
        Arc::new(F2fs::new()),
        Arc::new(Memfs::new()),
        Arc::new(Minfs::new()),
    ];
    const OP_SIZE: usize = 8 * 1024;
    const OP_COUNT: usize = 1024;

    let mut benchmark_set = BenchmarkSet::new();
    benchmark_set.add_benchmark(ReadSequentialCold::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(ReadSequentialWarm::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(ReadRandomCold::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(ReadRandomWarm::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(WriteSequentialCold::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(WriteSequentialWarm::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(WriteRandomCold::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(WriteRandomWarm::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(WriteSequentialFsyncCold::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(WriteSequentialFsyncWarm::new(OP_SIZE, OP_COUNT), &filesystems);
    benchmark_set.add_benchmark(StatPath::new(), &filesystems);
    benchmark_set.add_benchmark(OpenFile::new(), &filesystems);
    benchmark_set.add_benchmark(OpenDeeplyNestedFile::new(), &filesystems);

    // Creates a total of 62 directories and 189 files.
    let dts = DirectoryTreeStructure {
        files_per_directory: 3,
        directories_per_directory: 2,
        max_depth: 5,
    };
    benchmark_set.add_benchmark(WalkDirectoryTreeCold::new(dts, 20), &filesystems);
    benchmark_set.add_benchmark(WalkDirectoryTreeWarm::new(dts, 20), &filesystems);
    benchmark_set.add_benchmark(GitStatus::new(), &filesystems);

    benchmark_set
}

/// The blob benchmarks are separate from the others to prevent them from running in CQ until fxblob
/// supports writing compressed blobs.
fn build_blob_benchmark_set() -> BenchmarkSet {
    let mut benchmark_set = BenchmarkSet::new();
    let blob_filesystems: Vec<Arc<dyn FilesystemConfig>> =
        vec![Arc::new(Blobfs::new()), Arc::new(Fxblob::new())];

    const BLOB_SIZE: usize = 2 * 1024 * 1024; // 2 MiB
    benchmark_set
        .add_benchmark(PageInBlobSequentialUncompressed::new(BLOB_SIZE), &blob_filesystems);
    benchmark_set.add_benchmark(PageInBlobSequentialCompressed::new(BLOB_SIZE), &blob_filesystems);
    benchmark_set.add_benchmark(PageInBlobRandomCompressed::new(BLOB_SIZE), &blob_filesystems);

    benchmark_set
}

async fn load_blobs_for_tracing() {
    let filesystems: Vec<Arc<dyn FilesystemConfig>> = vec![
        Arc::new(Fxfs::new()),
        Arc::new(F2fs::new()),
        Arc::new(Memfs::new()),
        Arc::new(Minfs::new()),
    ];
    let ramdisk_factory = RamdiskFactory::new(4096, 32 * 1024).await;
    for fs in &filesystems {
        fs.start_filesystem(&ramdisk_factory).await.shutdown().await;
    }
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
    let benchmark_suite =
        if args.blob_benchmarks { build_blob_benchmark_set() } else { build_benchmark_set() };
    let results = benchmark_suite.run(&fvm_volume_factory, &filter).await;

    results.write_table(std::io::stdout());
    if args.output_csv {
        results.write_csv(std::io::stdout())
    }
    if let Some(path) = args.output_fuchsiaperf {
        let file = File::create(path).unwrap();
        results.write_fuchsia_perf_json(file);
    }
}
