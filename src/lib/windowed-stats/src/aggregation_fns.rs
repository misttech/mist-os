// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use num_traits::SaturatingAdd;

pub fn create_saturating_add_fn<T: SaturatingAdd>() -> Box<dyn Fn(&T, &T) -> T + Send> {
    Box::new(|value1, value2| value1.saturating_add(value2))
}
