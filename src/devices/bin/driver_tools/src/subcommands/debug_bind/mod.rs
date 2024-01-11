// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use {
    anyhow::{format_err, Result},
    args::DebugBindCommand,
    fidl_fuchsia_driver_development as fdd,
    std::io::Write,
};

pub async fn debug_bind(
    _cmd: DebugBindCommand,
    _writer: &mut dyn Write,
    _driver_development_proxy: fdd::ManagerProxy,
) -> Result<()> {
    // TODO(fxb:67441): Support the new bytecode format in the debugger.
    return Err(format_err!("The bind debugger is not supported currently."));
}
