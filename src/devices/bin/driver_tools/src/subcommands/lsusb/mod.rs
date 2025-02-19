// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use anyhow::Result;
use args::LsusbCommand;
use fidl_fuchsia_io as fio;

pub async fn lsusb(cmd: LsusbCommand, dev: &fio::DirectoryProxy) -> Result<()> {
    let (client, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let () = dev.open(
        &"class/usb-device",
        fio::PERM_READABLE | fio::Flags::PROTOCOL_DIRECTORY,
        &Default::default(),
        server_end.into_channel(),
    )?;
    lsusb::lsusb(client, cmd.into()).await
}
