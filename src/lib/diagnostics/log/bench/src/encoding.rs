// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_log_encoding::encode::Encoder;
use fidl_fuchsia_diagnostics_stream::{Argument, Value};
use fidl_fuchsia_logger::MAX_DATAGRAM_LEN_BYTES;
use fuchsia_criterion::{criterion, FuchsiaCriterion};
use std::io::Cursor;
use std::time::Duration;

#[inline]
fn encoder() -> Encoder<Cursor<[u8; MAX_DATAGRAM_LEN_BYTES as usize]>> {
    let buffer = [0u8; MAX_DATAGRAM_LEN_BYTES as usize];
    Encoder::new(Cursor::new(buffer))
}

fn bench_argument(value: Value) -> impl FnMut(&mut criterion::Bencher) + 'static {
    move |b: &mut criterion::Bencher| {
        let arg = Argument { name: "foo".to_string(), value: value.clone() };
        b.iter_batched_ref(
            || encoder(),
            |encoder| encoder.write_argument(&arg),
            criterion::BatchSize::SmallInput,
        );
    }
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
    .with_function("Encoder/Argument/Boolean", bench_argument(Value::Boolean(true)))
    .with_function("Encoder/Argument/Floating", bench_argument(Value::Floating(1234.5678)))
    .with_function("Encoder/Argument/UnsignedInt", bench_argument(Value::UnsignedInt(12345)))
    .with_function("Encoder/Argument/SignedInt", bench_argument(Value::SignedInt(-12345)));

    for size in [16, 128, 256, 512, 1024, 32000] {
        bench = bench.with_function(
            &format!("Encoder/Argument/Text/{}", size),
            bench_argument(Value::Text("x".repeat(size))),
        )
    }

    c.bench("fuchsia.diagnostics_log_rust.encoding", bench);
}
