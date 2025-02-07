// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use anyhow::{format_err, Context, Result};
use args::{BindCommand, DeviceCommand, DeviceSubcommand, RebindCommand, UnbindCommand};
use {fidl_fuchsia_device as fdev, fidl_fuchsia_io as fio};

pub async fn device(cmd: DeviceCommand, dev: fio::DirectoryProxy) -> Result<()> {
    match cmd.subcommand {
        DeviceSubcommand::Bind(BindCommand { ref device_path, ref driver_url_suffix }) => {
            let device = connect_to_device(dev, device_path)?;
            device.bind(driver_url_suffix).await?.map_err(|err| format_err!("{:?}", err))?;
            println!("Bound {} to {}", driver_url_suffix, device_path);
        }
        DeviceSubcommand::Unbind(UnbindCommand { ref device_path }) => {
            let device = connect_to_device(dev, device_path)?;
            device.schedule_unbind().await?.map_err(|err| format_err!("{:?}", err))?;
            println!("Unbound driver from {}", device_path);
        }
        DeviceSubcommand::Rebind(RebindCommand { ref device_path, ref driver_url_suffix }) => {
            let device = connect_to_device(dev, device_path).context("Failed to get device")?;
            device
                .rebind(driver_url_suffix)
                .await?
                .map_err(|err| format_err!("{:?}", err))
                .context("Failed to rebind")?;
            println!("Rebind of {} to {} is complete", driver_url_suffix, device_path);
        }
    }
    Ok(())
}

fn connect_to_device(dev: fio::DirectoryProxy, device_path: &str) -> Result<fdev::ControllerProxy> {
    // This should be fuchsia_component::client::connect_to_named_protocol_at_dir_root but this
    // needs to build on host for some reason.
    let (client, server) = fidl::endpoints::create_endpoints::<fio::NodeMarker>();
    let () =
        dev.deprecated_open(fio::OpenFlags::empty(), fio::ModeType::empty(), device_path, server)?;
    let client: fidl::endpoints::ClientEnd<fdev::ControllerMarker> = client.into_channel().into();
    Ok(client.into_proxy())
}
