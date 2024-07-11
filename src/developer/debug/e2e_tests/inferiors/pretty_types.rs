// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::vec::Vec;

extern "C" {
    // Defined in the linked in C++ library. We can't define a Rust type in the global namespace
    // since everything is implicitly in this module's namespace.
    static p: std::os::raw::c_ulong;
}

#[derive(Debug)]
struct NestedType {
    #[allow(dead_code)]
    num: u32,
}

#[derive(Debug)]
struct NestedVecs {
    pub input: Vec<NestedType>,
    pub output: Vec<NestedType>,
}

fn main() {
    // We should be able to pretty print |v| successfully, even though there's a type that is
    // shadowing the names the pretty printer is trying to traverse.
    let mut v = NestedVecs { input: Vec::new(), output: Vec::new() };

    v.input.push(NestedType { num: 1 });
    v.input.push(NestedType { num: 2 });
    v.input.push(NestedType { num: 3 });
    v.input.push(NestedType { num: 4 });

    v.output.push(NestedType { num: 6 });
    v.output.push(NestedType { num: 7 });
    v.output.push(NestedType { num: 8 });
    v.output.push(NestedType { num: 9 });

    // So that the global variable doesn't get optimized away.
    unsafe {
        println!("{:?}", p);
    }
}
