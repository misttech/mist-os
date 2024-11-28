// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Fuchsia-specific FIDL extensions.

mod decoder;
mod encoder;
mod transport;
mod wire;

pub use self::decoder::*;
pub use self::encoder::*;
pub use self::transport::*;
pub use self::wire::*;

pub use zx;
