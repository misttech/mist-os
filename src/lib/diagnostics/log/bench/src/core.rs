// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_log::{Publisher, PublisherOptions};
use fidl_fuchsia_logger::{LogSinkMarker, LogSinkRequest};
use fuchsia_async as fasync;
use fuchsia_criterion::{criterion, FuchsiaCriterion};
use fuchsia_zircon as zx;
use futures::StreamExt;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tracing::{info, span, Event, Metadata, Subscriber};

async fn setup_publisher() -> (zx::Socket, Publisher) {
    let (proxy, mut requests) =
        fidl::endpoints::create_proxy_and_stream::<LogSinkMarker>().unwrap();
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
    logging_fn: F,
) where
    F: FnMut() -> (),
    S: Subscriber + Send + Sync,
{
    // This thread constantly reads from the socket to attempt to minimize the number of dropped
    // logs from the benchmark.
    let reader_thread = std::thread::spawn(move || {
        let mut executor = fasync::LocalExecutor::new();
        let Some(socket) = socket else {
            return;
        };
        executor.run_singlethreaded(async move {
            let socket = fasync::Socket::from_socket(socket).expect("create async socket");
            let mut stream = socket.as_datagram_stream();
            while let Some(result) = stream.next().await {
                let _ = criterion::black_box(result);
            }
        });
    });
    tracing::subscriber::with_default(subscriber, || bencher.iter(logging_fn));
    reader_thread.join().expect("join thread");
}

// The benchmarks below measure the time it takes to write a log message when calling a macro
// to log. They set up different cases: just a string, a string with arguments, the same string
// but with the arguments formatted, etc. It'll measure the time it takes for the log to go
// through the tracing mechanisms, our encoder and finally writing to the socket.
fn setup_write_log_benchmarks<F, S>(
    name: &str,
    mut make_subscriber: F,
    benchmark: Option<criterion::Benchmark>,
) -> criterion::Benchmark
where
    F: FnMut() -> (Option<zx::Socket>, S) + 'static + Copy,
    S: Subscriber + Send + Sync,
{
    let all_args_bench = move |b: &mut criterion::Bencher| {
        let (socket, subscriber) = make_subscriber();
        write_log_benchmark(b, socket, subscriber, || {
            info!(
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
                info!("this is a log emitted from the benchmark");
            });
        })
        .with_function(&format!("Publisher/{}/MessageWithSomeArguments", name), move |b| {
            let (socket, subscriber) = make_subscriber();
            write_log_benchmark(b, socket, subscriber, || {
                info!(
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
                info!(
                    "this is a log emitted from the benchmark boolean={} int={} string={}",
                    true, -123456, "foobarbaz",
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

    let mut bench = setup_write_log_benchmarks(
        "Tracing",
        move || {
            let mut executor = fasync::LocalExecutor::new();
            let (socket, publisher) = executor.run_singlethreaded(setup_publisher());
            (Some(socket), publisher)
        },
        None,
    );
    bench = setup_write_log_benchmarks(
        "TracingNoOp",
        move || (None, NoOpSubscriber::default()),
        Some(bench),
    );

    c.bench("fuchsia.diagnostics_log_rust.core", bench);
}
