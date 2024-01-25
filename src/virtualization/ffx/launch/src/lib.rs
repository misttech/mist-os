// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result, ffx_core::ffx_plugin, ffx_guest_launch_args::LaunchArgs, ffx_writer::Writer,
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
};

#[ffx_plugin("guest_enabled")]
pub async fn guest_launch(
    #[ffx(machine = guest_cli::launch::LaunchResult)] writer: Writer,
    args: LaunchArgs,
    remote_control: RemoteControlProxy,
) -> Result<()> {
    let services = guest_cli::platform::HostPlatformServices::new(remote_control);

    // TODO(https://fxbug.dev/42068091): Remove when overnet supports duplicated socket handles.
    if !args.detach {
        println!("The ffx guest plugin doesn't support attaching to a running guest.");
        println!("Re-run using the -d flag to detach, or use the guest tool via fx shell.");
        println!("See https://fxbug.dev/42068091 for updates.");
        return Ok(());
    }

    let output = guest_cli::launch::handle_launch(&services, &args).await;
    if writer.is_machine() {
        writer.machine(&output)?;
    } else {
        writer.write(format!("{}\n", output))?;
    }
    Ok(())
}
