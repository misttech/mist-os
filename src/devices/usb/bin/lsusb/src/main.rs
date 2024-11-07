// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use lsusb::args::Args;

#[fuchsia_async::run_singlethreaded]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();
    let proxy = fuchsia_fs::directory::open_in_namespace(
        "/dev/class/usb-device",
        fuchsia_fs::Flags::empty(),
    )?;
    lsusb::lsusb(proxy, args).await
}
