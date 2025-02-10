// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fidl_next::{DecoderExt as _, EncoderExt as _, TakeFrom as _};
use {fidl_next_test_log as ftl_next, fidl_test_log as ftl};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng as _};

const INPUT_SIZES: [usize; 4] = [1, 10, 100, 1000];

pub trait Generate {
    fn generate(rng: &mut impl Rng) -> Self;
}

impl Generate for ftl::Address {
    fn generate(rng: &mut impl Rng) -> Self {
        Self { x0: rng.gen(), x1: rng.gen(), x2: rng.gen(), x3: rng.gen() }
    }
}

impl Generate for ftl_next::Address {
    fn generate(rng: &mut impl Rng) -> Self {
        Self { x0: rng.gen(), x1: rng.gen(), x2: rng.gen(), x3: rng.gen() }
    }
}

const USERID: [&str; 9] =
    ["-", "alice", "bob", "carmen", "david", "eric", "frank", "george", "harry"];
const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const TIMEZONE: [&str; 25] = [
    "-1200", "-1100", "-1000", "-0900", "-0800", "-0700", "-0600", "-0500", "-0400", "-0300",
    "-0200", "-0100", "+0000", "+0100", "+0200", "+0300", "+0400", "+0500", "+0600", "+0700",
    "+0800", "+0900", "+1000", "+1100", "+1200",
];
fn generate_date(rng: &mut impl Rng) -> String {
    format!(
        "{}/{}/{}:{}:{}:{} {}",
        rng.gen_range(1..=28),
        MONTHS[rng.gen_range(0..12)],
        rng.gen_range(1970..=2021),
        rng.gen_range(0..24),
        rng.gen_range(0..60),
        rng.gen_range(0..60),
        TIMEZONE[rng.gen_range(0..25)],
    )
}

const CODES: [u16; 63] = [
    100, 101, 102, 103, 200, 201, 202, 203, 204, 205, 206, 207, 208, 226, 300, 301, 302, 303, 304,
    305, 306, 307, 308, 400, 401, 402, 403, 404, 405, 406, 407, 408, 409, 410, 411, 412, 413, 414,
    415, 416, 417, 418, 421, 422, 423, 424, 425, 426, 428, 429, 431, 451, 500, 501, 502, 503, 504,
    505, 506, 507, 508, 510, 511,
];
const METHODS: [&str; 5] = ["GET", "POST", "PUT", "UPDATE", "DELETE"];
const ROUTES: [&str; 7] = [
    "/favicon.ico",
    "/css/index.css",
    "/css/font-awsome.min.css",
    "/img/logo-full.svg",
    "/img/splash.jpg",
    "/api/login",
    "/api/logout",
];
const PROTOCOLS: [&str; 4] = ["HTTP/1.0", "HTTP/1.1", "HTTP/2", "HTTP/3"];
fn generate_request(rng: &mut impl Rng) -> String {
    format!(
        "{} {} {}",
        METHODS[rng.gen_range(0..5)],
        ROUTES[rng.gen_range(0..7)],
        PROTOCOLS[rng.gen_range(0..4)],
    )
}

impl Generate for ftl::Log {
    fn generate(rng: &mut impl Rng) -> Self {
        Self {
            address: ftl::Address::generate(rng),
            identity: "-".into(),
            userid: USERID[rng.gen_range(0..USERID.len())].into(),
            date: generate_date(rng),
            request: generate_request(rng),
            code: CODES[rng.gen_range(0..CODES.len())],
            size: rng.gen_range(0..100_000_000),
        }
    }
}

impl Generate for ftl_next::Log {
    fn generate(rng: &mut impl Rng) -> Self {
        Self {
            address: ftl_next::Address::generate(rng),
            identity: "-".into(),
            userid: USERID[rng.gen_range(0..USERID.len())].into(),
            date: generate_date(rng),
            request: generate_request(rng),
            code: CODES[rng.gen_range(0..CODES.len())],
            size: rng.gen_range(0..100_000_000),
        }
    }
}

fn generate_vec<T: Generate>(rng: &mut impl Rng, size: usize) -> Vec<T> {
    let mut result = Vec::with_capacity(size);
    for _ in 0..size {
        result.push(T::generate(rng));
    }
    result
}

fn make_rng() -> StdRng {
    // Nothing up my sleeve: seed is first 32 bytes of pi
    StdRng::from_seed([
        0x32, 0x43, 0xF6, 0xA8, 0x88, 0x5A, 0x30, 0x8D, 0x31, 0x31, 0x98, 0xA2, 0xE0, 0x37, 0x07,
        0x34, 0x4A, 0x40, 0x93, 0x82, 0x22, 0x99, 0xF3, 0x1D, 0x00, 0x82, 0xEF, 0xA9, 0x8E, 0xC4,
        0xE6, 0xC8,
    ])
}

fn rust_bench_log(c: &mut Criterion, input_size: usize) {
    let mut rng = make_rng();
    let input = ftl::Logs { logs: generate_vec(&mut rng, input_size) };

    let mut decode_buf = Vec::new();
    let mut decode_handle_buf = Vec::new();
    fidl::encoding::Encoder::<'_, fidl::encoding::NoHandleResourceDialect>::encode::<ftl::Logs>(
        &mut decode_buf,
        &mut decode_handle_buf,
        &input,
    )
    .unwrap();

    c.bench_function(&format!("rust/encode/{input_size}"), move |b| {
        let mut buf = Vec::new();
        let mut handle_buf = Vec::new();
        b.iter(|| {
            buf.clear();
            handle_buf.clear();
            black_box(
                fidl::encoding::Encoder::<'_, fidl::encoding::NoHandleResourceDialect>::encode::<
                    ftl::Logs,
                >(&mut buf, &mut handle_buf, &input),
            )
            .unwrap();
        });
    });

    c.bench_function(
        &format!("rust/decode_natural/{input_size}"),
        move |b| {
            b.iter(|| {
                let mut out = ftl::Logs { logs: Vec::new() };

                black_box(fidl::encoding::Decoder::<'_, fidl::encoding::NoHandleResourceDialect>::decode_with_context::<ftl::Logs>(
                    fidl::encoding::Context {
                        wire_format_version: fidl::encoding::WireFormatVersion::V2,
                    },
                    &decode_buf,
                    &mut decode_handle_buf,
                    &mut out,
                )).unwrap();
            });
        }
    );
}

fn rust_next_bench_log(c: &mut Criterion, input_size: usize) {
    let mut rng = make_rng();
    let mut input = ftl_next::Logs { logs: generate_vec(&mut rng, input_size) };

    let mut decode_chunks = Vec::new();
    decode_chunks.encode_next(&mut input).unwrap();

    c.bench_function(&format!("rust_next/encode/{input_size}"), move |b| {
        let mut chunks = Vec::new();
        b.iter(|| {
            chunks.clear();
            black_box(chunks.encode_next(black_box(&mut input))).unwrap();
        });
    });

    let decode_wire_chunks = decode_chunks.clone();
    c.bench_function(&format!("rust_next/decode_wire/{input_size}"), move |b| {
        b.iter_batched(
            || decode_wire_chunks.clone(),
            |mut decode_chunks| {
                let mut chunks = decode_chunks.as_mut_slice();
                black_box(black_box(&mut chunks).decode_next::<ftl_next::WireLogs>()).unwrap();
            },
            criterion::BatchSize::SmallInput,
        );
    });

    c.bench_function(&format!("rust_next/decode_natural/{input_size}"), move |b| {
        b.iter_batched(
            || decode_chunks.clone(),
            |mut decode_chunks| {
                let mut chunks = decode_chunks.as_mut_slice();
                let value = black_box(&mut chunks).decode_next::<ftl_next::WireLogs>().unwrap();
                black_box(ftl_next::Logs::take_from(&value));
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_log(c: &mut Criterion) {
    for input_size in INPUT_SIZES {
        rust_bench_log(c, input_size);
        rust_next_bench_log(c, input_size);
    }
}

criterion_group!(benchmark, bench_log);
criterion_main!(benchmark);
