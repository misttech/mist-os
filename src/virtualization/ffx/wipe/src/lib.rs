// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result, ffx_core::ffx_plugin, ffx_guest_wipe_args::WipeArgs, ffx_writer::Writer,
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
};

#[ffx_plugin("guest_enabled")]
pub async fn guest_wipe(
    #[ffx(machine = guest_cli::wipe::WipeResult)] writer: Writer,
    args: WipeArgs,
    remote_control: RemoteControlProxy,
) -> Result<()> {
    let services = guest_cli::platform::HostPlatformServices::new(remote_control);
    let output = guest_cli::wipe::handle_wipe(&services, &args).await?;

    if writer.is_machine() {
        writer.machine(&output)?;
    } else {
        writer.write(format!("{}\n", output))?;
    }

    Ok(())
}
