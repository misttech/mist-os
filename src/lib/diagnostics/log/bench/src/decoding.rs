// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use diagnostics_log::LogEvent;
use diagnostics_log_encoding::encode::{Encoder, EncoderOpts, WriteEventParams};
use diagnostics_log_encoding::parse::{parse_argument, parse_record};
use diagnostics_log_encoding::{Argument, Value};
use fidl_fuchsia_logger::MAX_DATAGRAM_LEN_BYTES;
use fuchsia_criterion::{criterion, FuchsiaCriterion};
use std::fmt;
use std::io::Cursor;
use std::time::Duration;

mod common;

fn bench_argument(
    value: impl Into<Value<'static>>,
) -> impl FnMut(&mut criterion::Bencher) + 'static {
    let value = value.into();
    move |b: &mut criterion::Bencher| {
        let arg = Argument::new("foo", value.clone());
        let buffer = [0u8; MAX_DATAGRAM_LEN_BYTES as usize];
        let mut encoder = Encoder::new(Cursor::new(buffer), EncoderOpts::default());
        let _ = encoder.write_argument(arg);
        b.iter(|| parse_argument(encoder.inner().get_ref()))
    }
}

const ENCODE_SIZE: usize = 4096;

fn write(
    message: fmt::Arguments<'static>,
    key_values: &[&(dyn log::kv::Source + Send + Sync)],
) -> Encoder<Cursor<[u8; ENCODE_SIZE]>> {
    let mut builder = log::Record::builder();
    builder.level(log::Level::Info);
    for key_value in key_values {
        builder.key_values(key_value);
    }
    let record = builder.args(message).build();
    let buffer = [0u8; ENCODE_SIZE];
    let mut encoder = Encoder::new(Cursor::new(buffer), EncoderOpts::default());
    assert_matches!(
        encoder.write_event(WriteEventParams {
            event: LogEvent::new(&record),
            tags: &["some-tag"],
            metatags: std::iter::empty(),
            pid: *common::PROCESS_ID,
            tid: *common::THREAD_ID,
            dropped: 1,
        }),
        Ok(())
    );
    encoder
}

fn setup_read_event_benchmarks(bench: criterion::Benchmark) -> criterion::Benchmark {
    bench
        .with_function("Decoder/ReadEvent/AllArguments", |b| {
            let encoder = write(
                format_args!("this is a log emitted from the benchmark"),
                &[
                    &("tag", "logbench"),
                    &("boolean", true),
                    &("float", 1234.5678),
                    &("int", -123456),
                    &("string", "foobarbaz"),
                    &("uint", 123456),
                ],
            );
            b.iter(|| parse_record(encoder.inner().get_ref()).unwrap())
        })
        .with_function("Decoder/ReadEvent/NoArguments", |b| {
            let encoder = write(format_args!("this is a log emitted from the benchmark"), &[]);
            b.iter(|| parse_record(encoder.inner().get_ref()).unwrap())
        })
        .with_function("Decoder/ReadEvent/MessageAsString", |b| {
            let encoder = write(format_args!("this is a log emitted from the benchmark boolean=true int=98765 string=foobarbaz"), &[]);
            b.iter(|| parse_record(encoder.inner().get_ref()).unwrap())
        })
        .with_function("Decoder/ReadEvent/MessageWithSomeArguments", |b| {
            let encoder = write(
                format_args!("this is a log emitted from the benchmark"),
                &[
                    &("boolean", true),
                    &("int", 98765),
                    &("string", "foobarbaz"),
                ],
            );
            b.iter(|| parse_record(encoder.inner().get_ref()).unwrap())
        })
}

fn main() {
    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut criterion::Criterion = &mut c;
    *internal_c = std::mem::take(internal_c)
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(100))
        .sample_size(100);

    let mut bench = criterion::Benchmark::new("Decoder/Argument/Boolean", bench_argument(true))
        .with_function("Decoder/Argument/Floating", bench_argument(1234.5678_f64))
        .with_function("Decoder/Argument/UnsignedInt", bench_argument(12345_u64))
        .with_function("Decoder/Argument/SignedInt", bench_argument(-12345_i64));

    for size in [16, 128, 256, 512, 1024, 32000] {
        bench = bench.with_function(
            format!("Decoder/Argument/Text/{}", size),
            bench_argument((*common::PLACEHOLDER_TEXT).get(..size).unwrap()),
        )
    }

    bench = setup_read_event_benchmarks(bench);

    c.bench("fuchsia.diagnostics_log_rust.decoding", bench);
}
