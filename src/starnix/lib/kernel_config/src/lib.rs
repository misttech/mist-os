// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use rand::Rng;

/// Takes a `name` and generates a `String` suitable to use as the name of a component in a
/// collection.
///
/// Used to avoid creating children with the same name.
pub fn generate_kernel_name(name: &str) -> String {
    let random_id: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();
    name.to_owned() + "_" + &random_id
}
