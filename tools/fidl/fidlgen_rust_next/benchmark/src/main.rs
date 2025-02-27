// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod log;
mod mesh;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fidl_next::{DecoderExt as _, EncoderExt as _};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng as _};

pub trait Generate {
    fn generate(rng: &mut impl Rng) -> Self;
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

fn bench_rust<T, D>(c: &mut Criterion, name: &str, input: T, input_size: usize, default: D)
where
    T: fidl::encoding::TypeMarker,
    for<'a> &'a T: fidl::encoding::Encode<T, fidl::encoding::NoHandleResourceDialect>,
    T::Owned: fidl::encoding::Decode<T, fidl::encoding::NoHandleResourceDialect>,
    D: 'static + Fn() -> T::Owned,
{
    let mut decode_buf = Vec::new();
    let mut decode_handle_buf = Vec::new();
    fidl::encoding::Encoder::<'_, fidl::encoding::NoHandleResourceDialect>::encode::<T>(
        &mut decode_buf,
        &mut decode_handle_buf,
        &input,
    )
    .unwrap();

    c.bench_function(&format!("rust/{name}/encode/{input_size}x"), move |b| {
        let mut buf = Vec::new();
        let mut handle_buf = Vec::new();
        b.iter(|| {
            buf.clear();
            handle_buf.clear();
            black_box(
                fidl::encoding::Encoder::<'_, fidl::encoding::NoHandleResourceDialect>::encode::<T>(
                    &mut buf,
                    &mut handle_buf,
                    black_box(&input),
                ),
            )
            .unwrap();
        });
    });

    c.bench_function(
        &format!("rust/{name}/decode_natural/{input_size}x"),
        move |b| {
            b.iter(|| {
                let mut out = default();

                black_box(fidl::encoding::Decoder::<'_, fidl::encoding::NoHandleResourceDialect>::decode_with_context::<T>(
                    fidl::encoding::Context {
                        wire_format_version: fidl::encoding::WireFormatVersion::V2,
                    },
                    black_box(&decode_buf),
                    &mut decode_handle_buf,
                    &mut out,
                )).unwrap();
            });
        }
    );
}

fn bench_rust_next<T, W>(c: &mut Criterion, name: &str, mut input: T, input_size: usize)
where
    T: 'static + fidl_next::Encode<Vec<fidl_next::Chunk>> + fidl_next::TakeFrom<W>,
    W: for<'a> fidl_next::Decode<&'a mut [fidl_next::Chunk]>,
{
    let mut decode_chunks = Vec::new();
    decode_chunks.encode_next(&mut input).unwrap();

    c.bench_function(&format!("rust_next/{name}/encode/{input_size}x"), move |b| {
        let mut chunks = Vec::new();
        b.iter(|| {
            chunks.clear();
            black_box(chunks.encode_next(black_box(&mut input))).unwrap();
        });
    });

    let decode_wire_chunks = decode_chunks.clone();
    c.bench_function(&format!("rust_next/{name}/decode_wire/{input_size}x"), move |b| {
        b.iter_batched_ref(
            || decode_wire_chunks.clone(),
            |decode_chunks| {
                let mut chunks = black_box(decode_chunks).as_mut_slice();
                black_box((&mut chunks).decode_next::<W>()).unwrap();
            },
            criterion::BatchSize::SmallInput,
        );
    });

    c.bench_function(&format!("rust_next/{name}/decode_natural/{input_size}x"), move |b| {
        b.iter_batched_ref(
            || decode_chunks.clone(),
            |decode_chunks| {
                let mut chunks = black_box(decode_chunks).as_mut_slice();
                let value = (&mut chunks).decode_next::<W>().unwrap();
                black_box(T::take_from(&value));
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_log(c: &mut Criterion) {
    const INPUT_SIZES: [usize; 4] = [1, 10, 100, 1000];

    for input_size in INPUT_SIZES {
        bench_rust(c, "log", log::generate_input_rust(input_size), input_size, || {
            fidl_test_benchmark::Logs { logs: Vec::new() }
        });
        bench_rust(c, "mesh", mesh::generate_input_rust(input_size), input_size, || {
            fidl_test_benchmark::Mesh { triangles: Vec::new() }
        });

        bench_rust_next(c, "log", log::generate_input_rust_next(input_size), input_size);
        bench_rust_next(c, "mesh", mesh::generate_input_rust_next(input_size), input_size);
    }
}

criterion_group!(benchmark, bench_log);
criterion_main!(benchmark);
