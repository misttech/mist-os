// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Next-generation FIDL Rust bindings library.

#![deny(
    future_incompatible,
    missing_docs,
    nonstandard_style,
    unused,
    warnings,
    clippy::all,
    clippy::alloc_instead_of_core,
    clippy::missing_safety_doc,
    clippy::std_instead_of_core,
    // TODO: re-enable this lint after justifying unsafe blocks
    // clippy::undocumented_unsafe_blocks,
    rustdoc::broken_intra_doc_links,
    rustdoc::missing_crate_level_docs
)]
#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(test)]
#[macro_use]
mod testing;

mod chunk;
mod decode;
pub mod decoder;
mod encode;
pub mod encoder;
#[cfg(target_os = "fuchsia")]
pub mod fuchsia;
mod owned;
mod slot;
mod take;
mod wire;

pub use bitflags::bitflags;
pub use munge::munge;
pub use rend::{f32_le, f64_le, i16_le, i32_le, i64_le, u16_le, u32_le, u64_le};

pub use self::chunk::*;
pub use self::decode::*;
pub use self::decoder::{Decoder, DecoderExt};
pub use self::encode::*;
pub use self::encoder::{Encoder, EncoderExt};
pub use self::owned::*;
pub use self::slot::*;
pub use self::take::*;
pub use self::wire::*;
