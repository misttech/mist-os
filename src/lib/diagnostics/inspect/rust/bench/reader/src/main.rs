// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async as fasync;
use fuchsia_criterion::criterion;
use fuchsia_inspect::hierarchy::DiagnosticsHierarchyGetter;
use fuchsia_inspect::reader::snapshot::{Snapshot, SnapshotTree};
use fuchsia_inspect::{Inspector, InspectorConfig, NumericProperty};
use futures::FutureExt;
use std::sync::{Arc, Mutex};

const HIERARCHY_GENERATOR_SEED: u64 = 0;

enum InspectorState {
    Running,
    Done,
}

/// Start a worker thread that will continually update the given inspector at the given rate.
///
/// Returns a closure that when called cancels the thread, returning only when the thread has
/// exited.
fn start_inspector_update_thread(inspector: Inspector, changes_per_second: usize) -> impl FnOnce() {
    let state = Arc::new(Mutex::new(InspectorState::Running));
    let ret_state = state.clone();

    let child = inspector.root().create_child("test");
    let val = child.create_int("val", 0);

    let thread = std::thread::spawn(move || loop {
        let sleep_time =
            std::time::Duration::from_nanos(1_000_000_000u64 / changes_per_second as u64);
        std::thread::sleep(sleep_time);
        match *state.lock().unwrap() {
            InspectorState::Done => {
                break;
            }
            _ => {}
        }
        val.add(1);
    });

    return move || {
        {
            *ret_state.lock().unwrap() = InspectorState::Done;
        }
        thread.join().expect("join thread");
    };
}

/// Adds the given number of nodes as lazy nodes to the tree. Each lazy node will contain only one
/// int.
fn add_lazies(inspector: Inspector, num_nodes: usize) {
    let node = inspector.root().create_child("node");

    for i in 0..num_nodes {
        node.record_lazy_child(format!("child-{}", i), move || {
            let insp = Inspector::default();
            insp.root().record_int("int", 1);
            async move { Ok(insp) }.boxed()
        });
    }
}

/// This benchmark measures the performance of snapshotting an Inspect VMO of different sizes. This
/// VMO is being written concurrently at the given frequency. This is fundamental when reading
/// Inspect data in the archivist.
fn snapshot_bench(b: &mut criterion::Bencher, size: usize, frequency: usize) {
    let inspector = Inspector::new(InspectorConfig::default().size(size));
    let vmo = inspector.duplicate_vmo().expect("failed to duplicate vmo");
    let done_fn = start_inspector_update_thread(inspector, frequency);

    b.iter_with_large_drop(move || loop {
        if let Ok(snapshot) = Snapshot::try_once_from_vmo(&vmo) {
            break snapshot;
        }
    });

    done_fn();
}

/// This benchmark measures the performance of reading an Inspect VMO backed by a Tree server of
/// different sizes and containing lazy nodes. This VMO is being written concurrently at the given
/// frequency. This is fundamental when reading inspect data in the archivist.
fn snapshot_tree_bench(b: &mut criterion::Bencher, size: usize, frequency: usize) {
    let mut executor = fuchsia_async::LocalExecutor::new();

    let inspector = Inspector::new(InspectorConfig::default().size(size));
    let (proxy, tree_server_fut) =
        fuchsia_inspect_bench_utils::spawn_server(inspector.clone()).unwrap();
    let task = fasync::Task::spawn(tree_server_fut);

    let done_fn = start_inspector_update_thread(inspector.clone(), frequency);
    add_lazies(inspector, 4);

    b.iter_with_large_drop(|| loop {
        if let Ok(snapshot) = executor.run_singlethreaded(SnapshotTree::try_from(&proxy)) {
            break snapshot;
        }
    });

    done_fn();
    drop(proxy);
    executor.run_singlethreaded(task).unwrap();
}

/// This benchmark measures the performance of reading an Inspect VMO backed by a Tree server of
/// different sizes and containing lazy nodes.
/// This is fundamental when reading inspect data in the archivist.
fn uncontended_snapshot_tree_bench(b: &mut criterion::Bencher, size: usize) {
    let mut executor = fuchsia_async::LocalExecutor::new();

    let inspector = Inspector::new(InspectorConfig::default().size(size));
    let (proxy, tree_server_fut) = fuchsia_inspect_bench_utils::spawn_server(inspector).unwrap();
    let task = fasync::Task::local(tree_server_fut);

    b.iter_with_large_drop(|| loop {
        if let Ok(snapshot) = executor.run_singlethreaded(SnapshotTree::try_from(&proxy)) {
            break snapshot;
        }
    });

    drop(proxy);
    executor.run_singlethreaded(task).unwrap();
}

/// This benchmark measures the performance of snapshotting an Inspect VMO backed by a tree server
/// when the vmo is partially filled to the given bytes.
fn reader_snapshot_tree_vmo_bench(b: &mut criterion::Bencher, size: usize, filled_size: i64) {
    let mut executor = fuchsia_async::LocalExecutor::new();

    let inspector = Inspector::new(InspectorConfig::default().size(size));
    let (proxy, tree_server_fut) =
        fuchsia_inspect_bench_utils::spawn_server(inspector.clone()).unwrap();
    let task = fasync::Task::local(tree_server_fut);

    #[allow(clippy::collection_is_never_read)]
    let mut nodes = vec![];
    if filled_size > 0 {
        let ints_for_filling: i64 = filled_size / 16 - 1;
        for i in 0..ints_for_filling {
            nodes.push(inspector.root().create_int("i", i));
        }
    }

    b.iter_with_large_drop(|| loop {
        if let Ok(snapshot) = executor.run_singlethreaded(SnapshotTree::try_from(&proxy)) {
            break snapshot;
        }
    });

    drop(proxy);
    executor.run_singlethreaded(task).unwrap();
}

fn snapshot_and_parse_bench(b: &mut criterion::Bencher, size: usize) {
    let hierarchy_generator =
        fuchsia_inspect_bench_utils::filled_hierarchy_generator(HIERARCHY_GENERATOR_SEED, size);

    b.iter_with_large_drop(|| {
        let _hierarchy = hierarchy_generator.get_diagnostics_hierarchy();
    });
}

fn main() {
    let mut c = fuchsia_inspect_bench_utils::configured_criterion(
        fuchsia_inspect_bench_utils::CriterionConfig::default(),
    );

    // TODO(https://fxbug.dev/42119817): Implement benchmarks where the real size doesn't match the
    // inspector size.
    // TODO(https://fxbug.dev/42119817): Enforce threads starting before benches run.

    // SNAPSHOT BENCHMARKS
    let mut bench = criterion::Benchmark::new("Snapshot/4K/1hz", move |b| {
        snapshot_bench(b, 4096, 1);
    });
    bench = bench.with_function("Snapshot/4K/10hz", move |b| {
        snapshot_bench(b, 4096, 10);
    });
    bench = bench.with_function("Snapshot/4K/100hz", move |b| {
        snapshot_bench(b, 4096, 100);
    });
    bench = bench.with_function("Snapshot/4K/1khz", move |b| {
        snapshot_bench(b, 4096, 1000);
    });
    bench = bench.with_function("Snapshot/4K/10khz", move |b| {
        snapshot_bench(b, 4096, 10_000);
    });
    bench = bench.with_function("Snapshot/4K/100khz", move |b| {
        snapshot_bench(b, 4096, 100_000);
    });
    bench = bench.with_function("Snapshot/4K/1mhz", move |b| {
        snapshot_bench(b, 4096, 1_000_000);
    });

    bench = bench.with_function("Snapshot/256K/1hz", move |b| {
        snapshot_bench(b, 4096 * 64, 1);
    });
    bench = bench.with_function("Snapshot/256K/10hz", move |b| {
        snapshot_bench(b, 4096 * 64, 10);
    });
    bench = bench.with_function("Snapshot/256K/100hz", move |b| {
        snapshot_bench(b, 4096 * 64, 100);
    });
    bench = bench.with_function("Snapshot/256K/1khz", move |b| {
        snapshot_bench(b, 4096 * 64, 1000);
    });

    bench = bench.with_function("Snapshot/1M/1hz", move |b| {
        snapshot_bench(b, 4096 * 256, 1);
    });
    bench = bench.with_function("Snapshot/1M/10hz", move |b| {
        snapshot_bench(b, 4096 * 256, 10);
    });
    bench = bench.with_function("Snapshot/1M/100hz", move |b| {
        snapshot_bench(b, 4096 * 256, 100);
    });
    bench = bench.with_function("Snapshot/1M/1khz", move |b| {
        snapshot_bench(b, 4096 * 256, 1000);
    });

    // SNAPSHOT TREE BENCHMARKS

    bench = bench.with_function("SnapshotTree/4K/1hz", move |b| {
        snapshot_tree_bench(b, 4096, 1);
    });
    bench = bench.with_function("SnapshotTree/4K/10hz", move |b| {
        snapshot_tree_bench(b, 4096, 10);
    });
    bench = bench.with_function("SnapshotTree/4K/100hz", move |b| {
        snapshot_tree_bench(b, 4096, 100);
    });
    bench = bench.with_function("SnapshotTree/4K/1khz", move |b| {
        snapshot_tree_bench(b, 4096, 1000);
    });
    bench = bench.with_function("SnapshotTree/4K/10khz", move |b| {
        snapshot_tree_bench(b, 4096, 10_000);
    });
    bench = bench.with_function("SnapshotTree/4K/100khz", move |b| {
        snapshot_tree_bench(b, 4096, 100_000);
    });
    bench = bench.with_function("SnapshotTree/4K/1mhz", move |b| {
        snapshot_tree_bench(b, 4096, 1_000_000);
    });

    bench = bench.with_function("SnapshotTree/16K/1hz", move |b| {
        snapshot_tree_bench(b, 4096 * 4, 1);
    });
    bench = bench.with_function("SnapshotTree/16K/10hz", move |b| {
        snapshot_tree_bench(b, 4096 * 4, 10);
    });
    bench = bench.with_function("SnapshotTree/16K/100hz", move |b| {
        snapshot_tree_bench(b, 4096 * 4, 100);
    });
    bench = bench.with_function("SnapshotTree/16K/1khz", move |b| {
        snapshot_tree_bench(b, 4096 * 4, 1000);
    });

    bench = bench.with_function("SnapshotTree/256K/1hz", move |b| {
        snapshot_tree_bench(b, 4096 * 64, 1);
    });
    bench = bench.with_function("SnapshotTree/256K/10hz", move |b| {
        snapshot_tree_bench(b, 4096 * 64, 10);
    });
    bench = bench.with_function("SnapshotTree/256K/100hz", move |b| {
        snapshot_tree_bench(b, 4096 * 64, 100);
    });
    bench = bench.with_function("SnapshotTree/256K/1khz", move |b| {
        snapshot_tree_bench(b, 4096 * 64, 1000);
    });

    bench = bench.with_function("SnapshotTree/1M/1hz", move |b| {
        snapshot_tree_bench(b, 4096 * 256, 1);
    });
    bench = bench.with_function("SnapshotTree/1M/10hz", move |b| {
        snapshot_tree_bench(b, 4096 * 256, 10);
    });
    bench = bench.with_function("SnapshotTree/1M/100hz", move |b| {
        snapshot_tree_bench(b, 4096 * 256, 100);
    });
    bench = bench.with_function("SnapshotTree/1M/1khz", move |b| {
        snapshot_tree_bench(b, 4096 * 256, 1000);
    });

    // UNCONTENDED SNAPSHOT BENCHMARKS

    bench = bench.with_function("UncontendedSnapshotTree/4K", move |b| {
        uncontended_snapshot_tree_bench(b, 4096);
    });
    bench = bench.with_function("UncontendedSnapshotTree/16K", move |b| {
        uncontended_snapshot_tree_bench(b, 4096 * 4);
    });
    bench = bench.with_function("UncontendedSnapshotTree/256K", move |b| {
        uncontended_snapshot_tree_bench(b, 4096 * 64);
    });
    bench = bench.with_function("UncontendedSnapshotTree/1M", move |b| {
        uncontended_snapshot_tree_bench(b, 4096 * 256);
    });

    // PARTIALLY FILLED BENCHMARKS

    bench = bench.with_function("SnapshotTree/VMO0PercentFull/1M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256, 0);
    });
    bench = bench.with_function("SnapshotTree/VMO25PercentFull/1M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256, 4096 * 64);
    });
    bench = bench.with_function("SnapshotTree/VMO50PercentFull/1M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256, 4096 * 128);
    });
    bench = bench.with_function("SnapshotTree/VMO75PercentFull/1M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256, 4096 * 192);
    });
    bench = bench.with_function("SnapshotTree/VMO100PercentFull/1M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256, 4096 * 256);
    });

    bench = bench.with_function("SnapshotTree/VMO0PercentFull/32M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256 * 32, 0);
    });
    bench = bench.with_function("SnapshotTree/VMO25PercentFull/32M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256 * 32, 4096 * 64 * 32);
    });
    bench = bench.with_function("SnapshotTree/VMO50PercentFull/32M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256 * 32, 4096 * 128 * 32);
    });
    bench = bench.with_function("SnapshotTree/VMO75PercentFull/32M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256 * 32, 4096 * 192 * 32);
    });
    bench = bench.with_function("SnapshotTree/VMO100PercentFull/32M", move |b| {
        reader_snapshot_tree_vmo_bench(b, 4096 * 256 * 32, 4096 * 256 * 32);
    });

    for exponent in 1..=5 {
        // This benchmark takes a snapshot of a seedable randomly generated
        // inspect hierarchy in a vmo and then applies the given selectors
        // to the snapshot to filter it down.
        let size = 10i32.pow(exponent);
        bench = bench.with_function(format!("SnapshotAndParse/{}", size), move |b| {
            snapshot_and_parse_bench(b, size as usize);
        });
    }

    c.bench("fuchsia.rust_inspect.reader_benchmarks", bench);
}
