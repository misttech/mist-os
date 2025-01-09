// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_log::{Publisher, PublisherOptions};
use fidl_fuchsia_logger::{LogSinkMarker, LogSinkRequest, MAX_DATAGRAM_LEN_BYTES};
use fuchsia_async as fasync;
use fuchsia_criterion::{criterion, FuchsiaCriterion};
use futures::{AsyncReadExt, StreamExt};
use std::thread;
use std::time::Duration;

async fn setup_logger() -> (thread::JoinHandle<()>, Publisher) {
    let (proxy, mut requests) = fidl::endpoints::create_proxy_and_stream::<LogSinkMarker>();
    let task = fasync::Task::spawn(async move {
        let options = PublisherOptions::default()
            .tags(&["some-tag"])
            .wait_for_initial_interest(false)
            .listen_for_interest_updates(false)
            .use_log_sink(proxy);

        Publisher::new(options).unwrap()
    });
    let socket = match requests.next().await.unwrap().unwrap() {
        LogSinkRequest::ConnectStructured { socket, .. } => socket,
        _ => panic!("sink ctor sent the wrong message"),
    };
    let drain_socket = std::thread::spawn(|| {
        let mut executor = fasync::LocalExecutor::new();
        executor.run_singlethreaded(async move {
            let mut socket = fuchsia_async::Socket::from_socket(socket);
            // Constantly drain the socket.
            let mut buf = vec![0; MAX_DATAGRAM_LEN_BYTES as usize];
            loop {
                match socket.read(&mut buf).await {
                    Ok(_) => {}
                    Err(s) => panic!("Unexpected status {}", s),
                }
            }
        });
    });
    let publisher = task.await;
    (drain_socket, publisher)
}

fn write_log_benchmark<F>(bencher: &mut criterion::Bencher, mut logging_fn: F)
where
    F: FnMut(),
{
    bencher.iter_batched(
        || {},
        |_| logging_fn(),
        // Limiting the batch size to 100 should prevent the socket from running out of
        // space.
        criterion::BatchSize::NumIterations(100),
    );
}

// The benchmarks below measure the time it takes to write a log message when calling a macro
// to log. They set up different cases: just a string, a string with arguments, the same string
// but with the arguments formatted, etc. It'll measure the time it takes for the log to go
// through the tracing mechanisms, our encoder and finally writing to the socket.
fn setup_tracing_write_benchmarks(
    name: &str,
    benchmark: Option<criterion::Benchmark>,
) -> criterion::Benchmark {
    let all_args_bench = move |b: &mut criterion::Bencher| {
        write_log_benchmark(b, || {
            tracing::info!(
                tag = "logbench",
                boolean = true,
                float = 1234.5678,
                int = -123456,
                string = "foobarbaz",
                uint = 123456,
                "this is a log emitted from the benchmark"
            );
        });
    };
    let bench = if let Some(benchmark) = benchmark {
        benchmark.with_function(format!("Publisher/{}/AllArguments", name), all_args_bench)
    } else {
        criterion::Benchmark::new(format!("Publisher/{}/AllArguments", name), all_args_bench)
    };
    bench
        .with_function(format!("Publisher/{}/NoArguments", name), move |b| {
            write_log_benchmark(b, || {
                tracing::info!("this is a log emitted from the benchmark");
            });
        })
        .with_function(format!("Publisher/{}/MessageWithSomeArguments", name), move |b| {
            write_log_benchmark(b, || {
                tracing::info!(
                    boolean = true,
                    int = -123456,
                    string = "foobarbaz",
                    "this is a log emitted from the benchmark",
                );
            });
        })
        .with_function(format!("Publisher/{}/MessageAsString", name), move |b| {
            write_log_benchmark(b, || {
                tracing::info!(
                    "this is a log emitted from the benchmark boolean={} int={} string={}",
                    true,
                    -123456,
                    "foobarbaz",
                );
            });
        })
}

fn setup_log_write_benchmarks(name: &str, bench: criterion::Benchmark) -> criterion::Benchmark {
    bench
        .with_function(format!("Publisher/{}/NoArguments", name), move |b| {
            write_log_benchmark(b, || {
                log::info!("this is a log emitted from the benchmark");
            });
        })
        .with_function(format!("Publisher/{}/MessageAsString", name), move |b| {
            write_log_benchmark(b, || {
                log::info!(
                    "this is a log emitted from the benchmark boolean={} int={} string={}",
                    true,
                    -123456,
                    "foobarbaz",
                );
            });
        })
}

fn create_logger() -> (thread::JoinHandle<()>, Publisher) {
    let mut executor = fasync::LocalExecutor::new();
    let (drain_socket, publisher) = executor.run_singlethreaded(setup_logger());
    (drain_socket, publisher)
}

fn main() {
    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut criterion::Criterion = &mut c;
    *internal_c = std::mem::take(internal_c)
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(100))
        // We must reduce the sample size from the default of 100, otherwise
        // Criterion will sometimes override the 1ms + 500ms suggested times
        // and run for much longer.
        .sample_size(10);

    let (_drain_logs, logger) = create_logger();
    log::set_boxed_logger(Box::new(logger.clone())).expect("setp logger");
    tracing::subscriber::set_global_default(logger).expect("setup subscriber");

    let mut bench = setup_tracing_write_benchmarks("Tracing", None);
    bench = setup_log_write_benchmarks("Log", bench);

    c.bench("fuchsia.diagnostics_log_rust.core", bench);
}
