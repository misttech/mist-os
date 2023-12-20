// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

extern crate self as ffx_validation;

pub mod schema;
pub mod validate;

// TODO: Implement full example validation with Schema + serde Serialize & Deserialize
use serde as _;
