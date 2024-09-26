// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT.
// Generated from FIDL library `zither.multifile` by zither, a Fuchsia platform tool.

#![allow(unused_imports)]

use zerocopy::IntoBytes;

use crate::a::*;
use crate::b::*;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, IntoBytes, PartialEq)]
pub struct C {
    pub a: A,
    pub b2: B2,
}
