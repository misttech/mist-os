// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::f32::consts::PI;

static mut SOME_GLOBAL: i32 = 0;

#[inline(never)]
fn leaf_no_args() {
    unsafe {
        SOME_GLOBAL = 5;
    }
}

#[inline(never)]
fn nested_no_args() {
    leaf_no_args();
}

#[inline(never)]
fn print_hello() {
    let num: i32;
    unsafe {
        num = SOME_GLOBAL;
    }
    println!("Hello! SOME_GLOBAL = {}", num);
}

#[inline(never)]
fn return_global_plus_one() -> i32 {
    unsafe { SOME_GLOBAL + 1 }
}

#[inline(never)]
fn return_float() -> f32 {
    PI
}

#[inline(never)]
fn return_i32_box() -> Box<i32> {
    unsafe { Box::new(SOME_GLOBAL) }
}

fn main() {
    nested_no_args();
    print_hello();
    println!("{}", return_global_plus_one());
    println!("{}", return_i32_box());
    println!("{}", return_float());
}
