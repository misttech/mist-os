// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::blob_benchmarks::{
    OpenAndGetVmoContentBlobCold, OpenAndGetVmoContentBlobWarm, OpenAndGetVmoMetaFileCold,
    OpenAndGetVmoMetaFileWarm, PageInBlobRandomCompressed, PageInBlobSequentialCompressed,
    PageInBlobSequentialUncompressed, WriteBlob, WriteRealisticBlobs,
};
use fuchsia_storage_benchmarks_lib::block_devices::FvmVolumeFactory;
use fuchsia_storage_benchmarks_lib::filesystems::{
    Blobfs, F2fs, Fxblob, Fxfs, Memfs, Minfs, PkgDirTest,
};
use regex::{Regex, RegexSetBuilder};
use std::fs::File;
use std::path::PathBuf;
use storage_benchmarks::directory_benchmarks::{
    DirectoryTreeStructure, GitStatus, OpenDeeplyNestedFile, OpenFile, StatPath,
    WalkDirectoryTreeCold, WalkDirectoryTreeWarm,
};
use storage_benchmarks::io_benchmarks::{
    ReadRandomCold, ReadRandomWarm, ReadSequentialCold, ReadSequentialWarm, ReadSparseCold,
    WriteRandomCold, WriteRandomWarm, WriteSequentialCold, WriteSequentialFsyncCold,
    WriteSequentialFsyncWarm, WriteSequentialWarm,
};
use storage_benchmarks::{add_benchmarks, BenchmarkSet};

mod blob_benchmarks;
mod blob_loader;

const FXFS_VOLUME_SIZE: u64 = 60 * 1024 * 1024;

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

    /// pages in all of the blobs in the package and exits. Does not run any benchmarks.
    ///
    /// When trying to collect a trace immediately after modifying a filesystem or a benchmark, the
    /// start of the trace will be polluted with downloading the new blobs, writing the blobs, and
    /// then paging the blobs back in. Running the benchmarks with this flag once before running
    /// them again with tracing enabled will remove most of the blob loading from the start of the
    /// trace.
    #[argh(switch)]
    load_blobs_for_tracing: bool,
}

fn add_io_benchmarks(benchmark_set: &mut BenchmarkSet) {
    const OP_SIZE: usize = 8 * 1024;
    const OP_COUNT: usize = 1024;
    add_benchmarks!(
        benchmark_set,
        [
            ReadSequentialWarm::new(OP_SIZE, OP_COUNT),
            ReadRandomWarm::new(OP_SIZE, OP_COUNT),
            WriteSequentialCold::new(OP_SIZE, OP_COUNT),
            WriteSequentialWarm::new(OP_SIZE, OP_COUNT),
            WriteRandomCold::new(OP_SIZE, OP_COUNT),
            WriteRandomWarm::new(OP_SIZE, OP_COUNT),
            WriteSequentialFsyncCold::new(OP_SIZE, OP_COUNT),
            WriteSequentialFsyncWarm::new(OP_SIZE, OP_COUNT),
        ],
        [Fxfs::new(FXFS_VOLUME_SIZE), F2fs, Memfs, Minfs]
    );
    add_benchmarks!(
        benchmark_set,
        [
            ReadSequentialCold::new(OP_SIZE, OP_COUNT),
            ReadRandomCold::new(OP_SIZE, OP_COUNT),
            ReadSparseCold::new(OP_SIZE, OP_COUNT),
        ],
        [Fxfs::new(FXFS_VOLUME_SIZE), F2fs, Minfs]
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
            WalkDirectoryTreeWarm::new(dts, 20),
            GitStatus::new(),
        ],
        [Fxfs::new(FXFS_VOLUME_SIZE), F2fs, Memfs, Minfs]
    );
    add_benchmarks!(
        benchmark_set,
        [WalkDirectoryTreeCold::new(dts, 20)],
        [Fxfs::new(FXFS_VOLUME_SIZE), F2fs, Minfs]
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
    add_benchmarks!(
        benchmark_set,
        [
            OpenAndGetVmoContentBlobCold::new(),
            OpenAndGetVmoContentBlobWarm::new(),
            OpenAndGetVmoMetaFileCold::new(),
            OpenAndGetVmoMetaFileWarm::new(),
        ],
        [PkgDirTest::new_fxblob(), PkgDirTest::new_blobfs()]
    );
}

#[fuchsia::main(logging_tags = ["storage_benchmarks"])]
async fn main() {
    let args: Args = argh::from_env();

    let _loaded_blobs = blob_loader::BlobLoader::load_blobs().await;
    if args.load_blobs_for_tracing {
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
        log::warn!("Not running any tests -- neither FVM nor GPT could be found.");
        log::warn!("To run these test locally on an emulator, see the README.md.");
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
