// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod context;
mod error;

pub use context::*;
pub use error::*;

#[cfg(test)]
pub mod tests {
    pub const FFX_STR: &str = "I am an ffx error";
    pub const ERR_STR: &str = "I am not an ffx error";
}
