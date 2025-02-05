// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::LazyLock;
use zx::{self as zx, AsHandleRef};

pub static PLACEHOLDER_TEXT: LazyLock<String> = LazyLock::new(|| "x".repeat(32000));
pub static PROCESS_ID: LazyLock<zx::Koid> =
    LazyLock::new(|| fuchsia_runtime::process_self().get_koid().unwrap());
pub static THREAD_ID: LazyLock<zx::Koid> =
    LazyLock::new(|| fuchsia_runtime::thread_self().get_koid().unwrap());
