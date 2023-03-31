// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT.
// Generated from FIDL library `zither.multifile` by zither, a Fuchsia platform tool.

#![allow(unused_imports)]

use zerocopy::AsBytes;

use crate::a::*;

pub const B1: A = A::Member;

#[repr(C)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub struct B2 {
    pub a: A,
}
