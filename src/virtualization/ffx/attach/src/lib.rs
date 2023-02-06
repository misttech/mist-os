// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result, ffx_core::ffx_plugin, ffx_guest_attach_args::AttachArgs, ffx_writer::Writer,
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
};

#[ffx_plugin("guest_enabled")]
pub async fn guest_attach(
    #[ffx(machine = guest_cli::attach::AttachResult)] _writer: Writer,
    args: AttachArgs,
    _remote_control: RemoteControlProxy,
) -> Result<()> {
    // TODO(fxbug.dev/116879): Remove when overnet supports duplicated socket handles.
    println!("The ffx guest plugin doesn't support attaching to a running guest.");
    println!("Use the guest tool instead: `fx shell guest attach {}`", args.guest_type);
    println!("See fxbug.dev/116879 for updates.");
    return Ok(());

    // TODO(fxbug.dev/116879): Enable when overnet supports duplicated socket handles.
    #[allow(unreachable_code)]
    {
        let services = guest_cli::platform::HostPlatformServices::new(_remote_control);

        let output = guest_cli::attach::handle_attach(&services, &args).await?;
        if _writer.is_machine() {
            _writer.machine(&output)?;
        } else {
            _writer.write(format!("{}\n", output))?;
        }
        Ok(())
    }
}
