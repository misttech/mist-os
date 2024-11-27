// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_log::{Publisher, PublisherOptions};
use fidl_fuchsia_logger::{LogSinkMarker, LogSinkRequest};
use fuchsia_async as fasync;
use fuchsia_criterion::{criterion, FuchsiaCriterion};
use futures::StreamExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;
use std::time::Duration;
use tracing::{span, Event, Metadata, Subscriber};
use tracing_log::LogTracer;

async fn setup_publisher() -> (zx::Socket, Publisher) {
    let (proxy, mut requests) = fidl::endpoints::create_proxy_and_stream::<LogSinkMarker>();
    let task = fasync::Task::spawn(async move {
        let options = PublisherOptions::default()
            .tags(&["some-tag"])
            .wait_for_initial_interest(false)
            .listen_for_interest_updates(false)
            .use_log_sink(proxy);
        let publisher = Publisher::new(options).unwrap();
        publisher
    });
    let socket = match requests.next().await.unwrap().unwrap() {
        LogSinkRequest::ConnectStructured { socket, .. } => socket,
        _ => panic!("sink ctor sent the wrong message"),
    };
    let publisher = task.await;
    (socket, publisher)
}

fn write_log_benchmark<F, S>(
    bencher: &mut criterion::Bencher,
    socket: Option<zx::Socket>,
    subscriber: S,
    mut logging_fn: F,
) where
    F: FnMut() -> (),
    S: Subscriber + Send + Sync,
{
    tracing::subscriber::with_default(subscriber, || {
        if let Some(socket) = socket {
            let mut buf = vec![0; 2048];
            bencher.iter_batched(
                || {
                    // Drain the socket
                    loop {
                        match socket.read(&mut buf) {
                            Ok(_) => {}
                            Err(zx::Status::SHOULD_WAIT) => break,
                            Err(s) => panic!("Unexpected status {}", s),
                        }
                    }
                },
                |_| logging_fn(),
                // Limiting the batch size to 100 should prevent the socket from running out of
                // space.
                criterion::BatchSize::NumIterations(100),
            )
        } else {
            bencher.iter(logging_fn);
        }
    });
}

// The benchmarks below measure the time it takes to write a log message when calling a macro
// to log. They set up different cases: just a string, a string with arguments, the same string
// but with the arguments formatted, etc. It'll measure the time it takes for the log to go
// through the tracing mechanisms, our encoder and finally writing to the socket.
fn setup_tracing_write_benchmarks<F, S>(
    name: &str,
    make_subscriber: F,
    benchmark: Option<criterion::Benchmark>,
) -> criterion::Benchmark
where
    F: Fn() -> (Option<zx::Socket>, S) + 'static + Copy,
    S: Subscriber + Send + Sync,
{
    let all_args_bench = move |b: &mut criterion::Bencher| {
        let (socket, subscriber) = make_subscriber();
        write_log_benchmark(b, socket, subscriber, || {
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
        benchmark.with_function(&format!("Publisher/{}/AllArguments", name), all_args_bench)
    } else {
        criterion::Benchmark::new(&format!("Publisher/{}/AllArguments", name), all_args_bench)
    };
    bench
        .with_function(&format!("Publisher/{}/NoArguments", name), move |b| {
            let (socket, subscriber) = make_subscriber();
            write_log_benchmark(b, socket, subscriber, || {
                tracing::info!("this is a log emitted from the benchmark");
            });
        })
        .with_function(&format!("Publisher/{}/MessageWithSomeArguments", name), move |b| {
            let (socket, subscriber) = make_subscriber();
            write_log_benchmark(b, socket, subscriber, || {
                tracing::info!(
                    boolean = true,
                    int = -123456,
                    string = "foobarbaz",
                    "this is a log emitted from the benchmark",
                );
            });
        })
        .with_function(&format!("Publisher/{}/MessageAsString", name), move |b| {
            let (socket, subscriber) = make_subscriber();
            write_log_benchmark(b, socket, subscriber, || {
                tracing::info!(
                    "this is a log emitted from the benchmark boolean={} int={} string={}",
                    true,
                    -123456,
                    "foobarbaz",
                );
            });
        })
}

#[derive(Default)]
struct NoOpSubscriber {
    counter: AtomicUsize,
}

impl Drop for NoOpSubscriber {
    fn drop(&mut self) {
        assert_ne!(self.counter.swap(0, Ordering::SeqCst), 0);
    }
}

impl Subscriber for NoOpSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, _event: &Event<'_>) {
        let _: usize = self.counter.fetch_add(1, Ordering::Relaxed);
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}
}

fn setup_log_write_benchmarks<F, S>(
    name: &str,
    bench: criterion::Benchmark,
    make_subscriber: F,
) -> criterion::Benchmark
where
    F: Fn() -> (Option<zx::Socket>, S) + 'static + Copy,
    S: Subscriber + Send + Sync,
{
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        LogTracer::init().unwrap();
    });
    bench
        .with_function(&format!("Publisher/{}/NoArguments", name), move |b| {
            let (socket, subscriber) = make_subscriber();
            write_log_benchmark(b, socket, subscriber, || {
                log::info!("this is a log emitted from the benchmark");
            });
        })
        .with_function(&format!("Publisher/{}/MessageAsString", name), move |b| {
            let (socket, subscriber) = make_subscriber();
            write_log_benchmark(b, socket, subscriber, || {
                log::info!(
                    "this is a log emitted from the benchmark boolean={} int={} string={}",
                    true,
                    -123456,
                    "foobarbaz",
                );
            });
        })
}

fn create_real_tracing_subscriber() -> (Option<zx::Socket>, Publisher) {
    let mut executor = fasync::LocalExecutor::new();
    let (socket, publisher) = executor.run_singlethreaded(setup_publisher());
    (Some(socket), publisher)
}

fn create_noop_subscriber() -> (Option<zx::Socket>, NoOpSubscriber) {
    (None, NoOpSubscriber::default())
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

    let mut bench =
        setup_tracing_write_benchmarks("Tracing", &create_real_tracing_subscriber, None);
    bench = setup_tracing_write_benchmarks("TracingNoOp", &create_noop_subscriber, Some(bench));

    bench = setup_log_write_benchmarks("Log", bench, &create_real_tracing_subscriber);
    bench = setup_log_write_benchmarks("LogNoOp", bench, &create_noop_subscriber);

    c.bench("fuchsia.diagnostics_log_rust.core", bench);
}
