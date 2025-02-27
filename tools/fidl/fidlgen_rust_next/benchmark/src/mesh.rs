// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{generate_vec, make_rng, Generate};
use {fidl_next_test_benchmark as ftb_next, fidl_test_benchmark as ftb};

use rand::Rng;

impl Generate for ftb::Vector3 {
    fn generate(rng: &mut impl Rng) -> Self {
        Self { x: rng.gen(), y: rng.gen(), z: rng.gen() }
    }
}

impl Generate for ftb_next::Vector3 {
    fn generate(rng: &mut impl Rng) -> Self {
        Self { x: rng.gen(), y: rng.gen(), z: rng.gen() }
    }
}

impl Generate for ftb::Triangle {
    fn generate(rng: &mut impl Rng) -> Self {
        Self {
            v0: ftb::Vector3::generate(rng),
            v1: ftb::Vector3::generate(rng),
            v2: ftb::Vector3::generate(rng),
            normal: ftb::Vector3::generate(rng),
        }
    }
}

impl Generate for ftb_next::Triangle {
    fn generate(rng: &mut impl Rng) -> Self {
        Self {
            v0: ftb_next::Vector3::generate(rng),
            v1: ftb_next::Vector3::generate(rng),
            v2: ftb_next::Vector3::generate(rng),
            normal: ftb_next::Vector3::generate(rng),
        }
    }
}

pub fn generate_input_rust(input_size: usize) -> ftb::Mesh {
    let mut rng = make_rng();
    ftb::Mesh { triangles: generate_vec(&mut rng, input_size) }
}

pub fn generate_input_rust_next(input_size: usize) -> ftb_next::Mesh {
    let mut rng = make_rng();
    ftb_next::Mesh { triangles: generate_vec(&mut rng, input_size) }
}
