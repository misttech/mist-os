// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Typed wrappers for the basic protocol types.

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

mod buffer;
mod client;
mod endpoint;
mod method;
mod server;

pub use self::buffer::*;
pub use self::client::*;
pub use self::endpoint::*;
pub use self::method::*;
pub use self::server::*;
