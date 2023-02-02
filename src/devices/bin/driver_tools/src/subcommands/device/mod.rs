// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use {
    anyhow::{format_err, Context, Result},
    args::{
        BindCommand, DeviceCommand, DeviceSubcommand, LogLevelCommand, RebindCommand, UnbindCommand,
    },
    fidl_fuchsia_device as fdev, fidl_fuchsia_io as fio, fuchsia_zircon_status as zx,
};

pub async fn device(cmd: DeviceCommand, dev: fio::DirectoryProxy) -> Result<()> {
    match cmd.subcommand {
        DeviceSubcommand::Bind(BindCommand { ref device_path, ref driver_path }) => {
            let device = connect_to_device(dev, device_path)?;
            device.bind(driver_path).await?.map_err(|err| format_err!("{:?}", err))?;
            println!("Bound {} to {}", driver_path, device_path);
        }
        DeviceSubcommand::Unbind(UnbindCommand { ref device_path }) => {
            let device = connect_to_device(dev, device_path)?;
            device.schedule_unbind().await?.map_err(|err| format_err!("{:?}", err))?;
            println!("Unbound driver from {}", device_path);
        }
        DeviceSubcommand::Rebind(RebindCommand { ref device_path, ref driver_path }) => {
            let device = connect_to_device(dev, device_path).context("Failed to get device")?;
            device
                .rebind(driver_path)
                .await?
                .map_err(|err| format_err!("{:?}", err))
                .context("Failed to rebind")?;
            println!("Rebind of {} to {} is complete", driver_path, device_path);
        }
        DeviceSubcommand::LogLevel(LogLevelCommand { ref device_path, log_level }) => {
            let device = connect_to_device(dev, device_path)?;
            if let Some(log_level) = log_level {
                zx::Status::ok(device.set_min_driver_log_severity(log_level.clone().into()).await?)
                    .map_err(|err| format_err!("{:?}", err))?;
                println!("Set {} log level to {}", device_path, log_level);
            } else {
                let (status, severity) = device.get_min_driver_log_severity().await?;
                zx::Status::ok(status).map_err(|err| format_err!("{:?}", err))?;
                println!("Current log severity: {:#?}", severity);
            }
        }
    }
    Ok(())
}

fn connect_to_device(dev: fio::DirectoryProxy, device_path: &str) -> Result<fdev::ControllerProxy> {
    // This should be fuchsia_component::client::connect_to_named_protocol_at_dir_root but this
    // needs to build on host for some reason.
    let (client, server) = fidl::endpoints::create_endpoints::<fio::NodeMarker>()?;
    let () = dev.open(fio::OpenFlags::empty(), fio::ModeType::empty(), device_path, server)?;
    let client: fidl::endpoints::ClientEnd<fdev::ControllerMarker> = client.into_channel().into();
    client.into_proxy().map_err(Into::into)
}
