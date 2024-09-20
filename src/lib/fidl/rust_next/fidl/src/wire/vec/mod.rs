// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod optional;
mod raw;
mod required;

pub use self::optional::WireOptionalVector;
pub use self::required::WireVector;
