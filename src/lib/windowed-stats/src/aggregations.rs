// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {num_traits::SaturatingAdd, std::ops::Add};

pub fn create_saturating_add_fn<T: SaturatingAdd>() -> Box<dyn Fn(&T, &T) -> T + Send> {
    Box::new(|value1, value2| value1.saturating_add(value2))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SumAndCount {
    pub sum: u32,
    pub count: u32,
}

impl SumAndCount {
    pub fn avg(&self) -> f64 {
        self.sum as f64 / self.count as f64
    }
}

// `Add` implementation is required to implement `SaturatingAdd` down below.
impl Add for SumAndCount {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self { sum: self.sum + other.sum, count: self.count + other.count }
    }
}

impl SaturatingAdd for SumAndCount {
    fn saturating_add(&self, other: &Self) -> Self {
        Self {
            sum: self.sum.saturating_add(other.sum),
            count: self.count.saturating_add(other.count),
        }
    }
}
