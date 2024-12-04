// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_log_encoding::encode::{
    Encoder, EncoderOpts, TracingEvent, WriteArgumentValue, WriteEventParams,
};
use fidl_fuchsia_logger::MAX_DATAGRAM_LEN_BYTES;
use fuchsia_criterion::{criterion, FuchsiaCriterion};
use std::io::Cursor;
use std::time::Duration;
use tracing::{Event, Metadata};
use tracing_core::field;
use tracing_subscriber::Registry;

mod common;

type TestBuffer = Cursor<[u8; MAX_DATAGRAM_LEN_BYTES as usize]>;
type TestEncoder = Encoder<TestBuffer>;

#[inline]
fn encoder() -> TestEncoder {
    let buffer = [0u8; MAX_DATAGRAM_LEN_BYTES as usize];
    Encoder::new(Cursor::new(buffer), EncoderOpts::default())
}

fn bench_argument<T>(value: T) -> impl FnMut(&mut criterion::Bencher) + 'static
where
    T: WriteArgumentValue<TestBuffer> + Copy + 'static,
{
    move |b: &mut criterion::Bencher| {
        b.iter_batched_ref(
            encoder,
            |encoder| encoder.write_raw_argument("foo", value),
            criterion::BatchSize::SmallInput,
        );
    }
}

// `tracing_core::field::private::ValidLen<'_>` is not implemented for `[(&tracing_core::Field,
// std::option::Option<&dyn tracing::Value>); N]`
// Therefore we cannot use const generics for doing this.
macro_rules! impl_bench_write_event {
    ($($n:tt),+) => {
        $(
            paste::paste! {
                fn [< bench_write_event_with_ $n _args>](
                    b: &mut criterion::Bencher,
                    metadata: &'static Metadata<'static>,
                    fields: [field::Field; $n],
                    values: [&dyn field::Value; $n],
                ) {
                    let value_set_entries = common::make_value_set(&fields, &values);
                    let value_set = metadata.fields().value_set(&value_set_entries);
                    let event = Event::new(metadata, &value_set);
                    b.iter_batched_ref(
                        || encoder(),
                        |encoder| encoder.write_event(
                            WriteEventParams {
                            event: TracingEvent::<Registry>::from_event(&event),
                            tags: &["some-tag"],
                            metatags: std::iter::empty(),
                            pid: *common::PROCESS_ID,
                            tid: *common::THREAD_ID,
                            dropped: 1
                        }),
                        criterion::BatchSize::SmallInput,
                    )
                }
            }
        )+
    }
}

impl_bench_write_event!(1, 4, 7);

fn setup_write_event_benchmarks(bench: criterion::Benchmark) -> criterion::Benchmark {
    bench
        .with_function("Encoder/WriteEvent/AllArguments", |b| {
            let (metadata, fields, values) = common::make_event_metadata!(
                message: "this is a log emitted from the benchmark",
                tag: "logbench",
                boolean: true,
                float: 1234.5678,
                int: -123456,
                string: "foobarbaz",
                uint: 123456
            );
            bench_write_event_with_7_args(b, metadata, fields, values);
        })
        .with_function("Encoder/WriteEvent/NoArguments", |b| {
            let (metadata, fields, values) = common::make_event_metadata!(
                message: "this is a log emitted from the benchmark"
            );
            bench_write_event_with_1_args(b, metadata, fields, values);
        })
        .with_function("Encoder/WriteEvent/MessageAsString", |b| {
            let (metadata, fields, values) = common::make_event_metadata!(
                message:
                    // NOTE: the arguments here should match the bench below.
                    concat!(
                        "this is a log emitted from the benchmark boolean=true ",
                        "int=98765 string=foobarbaz"
                    )
            );
            bench_write_event_with_1_args(b, metadata, fields, values);
        })
        .with_function("Encoder/WriteEvent/MessageWithSomeArguments", |b| {
            let (metadata, fields, values) = common::make_event_metadata!(
                message: "this is a log emitted from the benchmark",
                boolean: true,
                int: 98765,
                string: "foobarbaz"
            );
            bench_write_event_with_4_args(b, metadata, fields, values);
        })
}

fn main() {
    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut criterion::Criterion = &mut c;
    *internal_c = std::mem::take(internal_c)
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(100))
        // We must reduce the sample size from the default of 100, otherwise
        // Criterion will sometimes override the 1ms + 100ms suggested times
        // and run for much longer.
        .sample_size(10);

    let mut bench = criterion::Benchmark::new("Encoder/Create", move |b| {
        b.iter_with_large_drop(encoder);
    })
    .with_function("Encoder/Argument/Boolean", bench_argument(true))
    .with_function("Encoder/Argument/Floating", bench_argument(1234.5678_f64))
    .with_function("Encoder/Argument/UnsignedInt", bench_argument(12345_u64))
    .with_function("Encoder/Argument/SignedInt", bench_argument(-12345_i64));

    for size in [16, 128, 256, 512, 1024, 32000] {
        bench = bench.with_function(
            format!("Encoder/Argument/Text/{}", size),
            bench_argument((*common::PLACEHOLDER_TEXT).get(..size).unwrap()),
        )
    }

    bench = setup_write_event_benchmarks(bench);

    c.bench("fuchsia.diagnostics_log_rust.encoding", bench);
}
