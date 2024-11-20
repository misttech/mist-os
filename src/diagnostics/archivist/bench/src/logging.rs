// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_criterion::criterion::{self, Criterion};
use fuchsia_criterion::FuchsiaCriterion;

use archivist_lib::identity::ComponentIdentity;
use archivist_lib::logs::shared_buffer::{LazyItem, SharedBuffer};
use archivist_lib::logs::stored_message::StoredMessage;
use diagnostics_log_encoding::encode::{Encoder, EncoderOpts};
use diagnostics_log_encoding::{Argument, Record, Severity as StreamSeverity};
use fidl_fuchsia_diagnostics::StreamMode;
use fuchsia_async as fasync;
use futures::StreamExt;
use std::convert::{TryFrom, TryInto};
use std::io::Cursor;
use std::mem;
use std::pin::pin;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

fn make_message(msg: &str, timestamp: zx::BootInstant) -> StoredMessage {
    let record = Record {
        timestamp,
        severity: StreamSeverity::Debug.into_primitive(),
        arguments: vec![
            Argument::pid(zx::Koid::from_raw(1)),
            Argument::tid(zx::Koid::from_raw(2)),
            Argument::message(msg),
        ],
    };
    let mut buffer = Cursor::new(vec![0u8; msg.len() + 128]);
    let mut encoder = Encoder::new(&mut buffer, EncoderOpts::default());
    encoder.write_record(record).unwrap();
    let encoded = &buffer.get_ref()[..buffer.position() as usize];
    StoredMessage::new(encoded.to_vec().into(), &Default::default()).unwrap()
}

fn get_component_identity() -> Arc<ComponentIdentity> {
    Arc::new(ComponentIdentity::new(moniker::Moniker::try_from(vec!["a"]).unwrap().into(), ""))
}

fn bench_fill(b: &mut criterion::Bencher, size: usize) {
    // SharedBuffer needs an executor even though we don't use it in the benchmark.
    let _executor = fasync::SendExecutor::new(1);
    let buffer = Arc::new(SharedBuffer::new(65536, Box::new(|_| {})));
    let msg =
        make_message(std::str::from_utf8(&[65; 100]).unwrap(), zx::BootInstant::from_nanos(1));
    let container = Arc::new(buffer.new_container_buffer(get_component_identity(), Arc::default()));
    b.iter(|| {
        for _ in 0..size {
            container.push_back(msg.bytes());
        }
    });
}

#[derive(Copy, Clone)]
struct IterateArgs {
    size: usize,
    write_threads: usize,
    writes_per_second: usize,
}

fn bench_iterate_concurrent(b: &mut criterion::Bencher, args: IterateArgs) {
    let mut executor = fasync::SendExecutor::new(1);
    let done = Arc::new(AtomicBool::new(false));
    // Messages take up a a little less than 200 bytes in the buffer.
    let buffer = Arc::new(SharedBuffer::new(200 * args.size, Box::new(|_| {})));
    let msg = Arc::new(make_message(
        std::str::from_utf8(&[65; 100]).unwrap(),
        zx::BootInstant::from_nanos(1),
    ));
    let container = Arc::new(buffer.new_container_buffer(get_component_identity(), Arc::default()));

    for _ in 0..args.size {
        // fill the list
        container.push_back(msg.bytes());
    }

    // create writer threads that constantly pop and push values
    // the overall size of the list should remain approximately the same
    let mut threads = vec![];
    for _ in 0..args.write_threads {
        let done = done.clone();
        let container = Arc::clone(&container);
        let msg = Arc::clone(&msg);
        threads.push(std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                container.push_back(msg.bytes());
                std::thread::sleep(Duration::from_micros(
                    (1_000_000 / args.writes_per_second).try_into().unwrap(),
                ));
            }
        }));
    }

    // measure how long it takes to read |size| entries from the list
    b.iter(|| {
        let container = Arc::clone(&container);
        executor.run(async move {
            let mut items_read = 0;
            let mut cursor = pin!(container.cursor(StreamMode::SnapshotThenSubscribe).unwrap());
            while items_read < args.size {
                match cursor.next().await.expect("must have some value") {
                    LazyItem::Next(_) => {
                        items_read += 1;
                    }
                    _ => {}
                }
            }
        });
    });

    done.store(true, std::sync::atomic::Ordering::Relaxed);

    for thread in threads {
        thread.join().unwrap();
    }
}

fn main() {
    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut Criterion = &mut c;
    *internal_c = mem::take(internal_c)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(2))
        .sample_size(20);

    // The following benchmarks measure the performance of SharedBuffer, the fundamental data
    // structured used to store components' logs.

    // Benchmark the time needed to fill the buffer with just 100 entries.  This won't cause
    // wrapping.
    let mut bench = criterion::Benchmark::new("Logging/SharedBuffer/Fill/100", move |b| {
        bench_fill(b, 100);
    });

    // Benchmark the time needed to add 16K entries.
    // This measures the performance of rotating log buffers.
    bench = bench.with_function("Logging/SharedBuffer/Fill/16K", move |b| {
        bench_fill(b, 16 * 1024);
    });

    // Benchmark the time needed to read 16K entries from an ArcList starting with 16K entries
    // while concurrent threads are writing to the list.
    // This measures the scenario of reading a fixed amount of logs while numerous components
    // continue writing and rotating logs to the buffer.

    // This benchmark has no concurrent writers, so it measures the baseline to read all 16K logs
    // out of the buffer.
    bench = bench.with_function(
        "Logging/SharedBuffer/Iterate/size=16K/writers=0/per_second=1",
        move |b| {
            bench_iterate_concurrent(
                b,
                IterateArgs { size: 16 * 1024, write_threads: 0, writes_per_second: 1 },
            );
        },
    );

    // This benchmark has one concurrent writer pushing 50 logs per second.
    // It measures the overhead of concurrent write locking on the reader.
    bench = bench.with_function(
        "Logging/SharedBuffer/Iterate/size=16K/writers=1/per_second=50",
        move |b| {
            bench_iterate_concurrent(
                b,
                IterateArgs { size: 16 * 1024, write_threads: 1, writes_per_second: 50 },
            );
        },
    );

    // Same as above, but with 500 logs per second being written.
    bench = bench.with_function(
        "Logging/SharedBuffer/Iterate/size=16K/writers=1/per_second=500",
        move |b| {
            bench_iterate_concurrent(
                b,
                IterateArgs { size: 16 * 1024, write_threads: 1, writes_per_second: 500 },
            );
        },
    );

    // Same as above, but with 3 threads each writing 500 logs per second.
    bench = bench.with_function(
        "Logging/SharedBuffer/Iterate/size=16K/writers=3/per_second=500",
        move |b| {
            bench_iterate_concurrent(
                b,
                IterateArgs { size: 16 * 1024, write_threads: 3, writes_per_second: 500 },
            );
        },
    );

    c.bench("fuchsia.archivist", bench);
}
