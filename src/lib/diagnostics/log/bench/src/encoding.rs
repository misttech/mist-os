// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_log_encoding::encode::{Argument, Encoder, TracingEvent, Value, WriteEventParams};
use fidl_fuchsia_logger::MAX_DATAGRAM_LEN_BYTES;
use fuchsia_criterion::{criterion, FuchsiaCriterion};
use fuchsia_zircon::{self as zx, AsHandleRef};
use once_cell::sync::Lazy;
use std::{io::Cursor, mem::MaybeUninit, time::Duration};
use tracing::{Callsite, Event, Level, Metadata};
use tracing_core::{field, identify_callsite, subscriber::Interest, Kind};
use tracing_subscriber::Registry;

static PLACEHOLDER_TEXT: Lazy<String> = Lazy::new(|| "x".repeat(32000));
static PROCESS_ID: Lazy<zx::Koid> =
    Lazy::new(|| fuchsia_runtime::process_self().get_koid().unwrap());
static THREAD_ID: Lazy<zx::Koid> = Lazy::new(|| fuchsia_runtime::thread_self().get_koid().unwrap());

struct NullCallsite;
static NULL_CALLSITE: NullCallsite = NullCallsite;
impl Callsite for NullCallsite {
    fn set_interest(&self, _: Interest) {
        unreachable!("unused by the encoder")
    }

    fn metadata(&self) -> &Metadata<'_> {
        unreachable!("unused by the encoder")
    }
}

macro_rules! len {
    () => (0usize);
    ($head:ident $($tail:ident)*) => (1usize + len!($($tail)*));
}

// Generates the metadata required to create a tracing::Event with the lifetimes that it expects.
macro_rules! make_event_metadata {
    ($($key:ident: $value:expr),*) => {{
        static METADATA: Metadata<'static> = Metadata::new(
            /* name */ "benching",
            /* target */ "bench",
            Level::INFO,
            Some(file!()),
            Some(line!()),
            Some(module_path!()),
            field::FieldSet::new(
                &[$( stringify!($key),)*],
                identify_callsite!(&NULL_CALLSITE),
            ),
            Kind::EVENT,
        );
        const N : usize = len!($($key)*);
        let fields : [field::Field; N] = [$(METADATA.fields().field(stringify!($key)).unwrap(),)*];
        let values : [&dyn field::Value; N] = [$((&$value as &dyn field::Value),)*];
        (&METADATA, fields, values)
    }};
}

type ValueSetItem<'a> = (&'a field::Field, Option<&'a (dyn field::Value + 'a)>);

// TODO: this will be much cleaner when Array zip is stable.
// https://github.com/rust-lang/rust/issues/80094
fn make_value_set<'a, const N: usize>(
    fields: &'a [field::Field; N],
    values: &[&'a (dyn field::Value + 'a); N],
) -> [ValueSetItem<'a>; N] {
    // Safety: assume_init is safe because the type is MaybeUninit which
    // doesn't need to be initialized.
    // Use uninit_array when stabilized.
    // https://doc.rust-lang.org/stable/src/core/mem/maybe_uninit.rs.html#350
    let mut data: [MaybeUninit<ValueSetItem<'a>>; N] =
        unsafe { MaybeUninit::uninit().assume_init() };
    for (i, slot) in data.iter_mut().enumerate() {
        slot.write((&fields[i], Some(values[i])));
    }
    // See https://github.com/rust-lang/rust/issues/61956 for why we can't use transmute.
    // Safety: everything has been initialized now.
    unsafe { std::mem::transmute_copy::<_, [ValueSetItem<'a>; N]>(&data) }
}

#[inline]
fn encoder() -> Encoder<Cursor<[u8; MAX_DATAGRAM_LEN_BYTES as usize]>> {
    let buffer = [0u8; MAX_DATAGRAM_LEN_BYTES as usize];
    Encoder::new(Cursor::new(buffer))
}

fn bench_argument(
    value: impl Into<Value<'static>>,
) -> impl FnMut(&mut criterion::Bencher) + 'static {
    let value = value.into();
    move |b: &mut criterion::Bencher| {
        let arg = Argument { name: "foo", value: value.clone() };
        b.iter_batched_ref(
            || encoder(),
            |encoder| encoder.write_argument(&arg),
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
                    let value_set_entries = make_value_set(&fields, &values);
                    let value_set = metadata.fields().value_set(&value_set_entries);
                    let event = Event::new(metadata, &value_set);
                    b.iter_batched_ref(
                        || encoder(),
                        |encoder| encoder.write_event(
                            WriteEventParams {
                            event: TracingEvent::<Registry>::from_event(&event),
                            tags: &["some-tag"],
                            metatags: std::iter::empty(),
                            pid: *PROCESS_ID,
                            tid: *THREAD_ID,
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
            let (metadata, fields, values) = make_event_metadata!(
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
            let (metadata, fields, values) = make_event_metadata!(
                message: "this is a log emitted from the benchmark"
            );
            bench_write_event_with_1_args(b, metadata, fields, values);
        })
        .with_function("Encoder/WriteEvent/MessageAsString", |b| {
            let (metadata, fields, values) = make_event_metadata!(
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
            let (metadata, fields, values) = make_event_metadata!(
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
        b.iter_with_large_drop(|| encoder());
    })
    .with_function("Encoder/Argument/Boolean", bench_argument(true))
    .with_function("Encoder/Argument/Floating", bench_argument(1234.5678 as f64))
    .with_function("Encoder/Argument/UnsignedInt", bench_argument(12345 as u64))
    .with_function("Encoder/Argument/SignedInt", bench_argument(-12345 as i64));

    for size in [16, 128, 256, 512, 1024, 32000] {
        bench = bench.with_function(
            &format!("Encoder/Argument/Text/{}", size),
            bench_argument((*PLACEHOLDER_TEXT).get(..size).unwrap()),
        )
    }

    bench = setup_write_event_benchmarks(bench);

    c.bench("fuchsia.diagnostics_log_rust.encoding", bench);
}
