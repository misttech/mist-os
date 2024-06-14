// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod constants;
mod fields;
mod id;
mod parse;
mod reader;

pub use constants::*;
pub use fields::*;
pub use id::*;
pub use parse::*;
pub use reader::*;

use crate::big_endian::BigEndianU16;
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell, Unaligned};

#[repr(C, packed)]
#[derive(AsBytes, FromZeros, FromBytes, NoCell, Unaligned)]
pub struct AttributeHeader {
    id: Id,
    body_len: BigEndianU16,
}
