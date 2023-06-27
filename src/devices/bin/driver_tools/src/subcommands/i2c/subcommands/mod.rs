// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod ping;
pub mod read;
pub mod transact;
pub mod write;

use {
    anyhow::{Context as _, Result},
    fidl_fuchsia_hardware_i2c as fi2c, fidl_fuchsia_io as fio, fuchsia_zircon_status as zx,
    std::path::Path,
};

fn connect_to_i2c_device(
    device_path: impl AsRef<Path>,
    root: &fio::DirectoryProxy,
) -> Result<fi2c::DeviceProxy> {
    let device_path =
        device_path.as_ref().to_str().ok_or(anyhow::anyhow!("Failed to get device path string"))?;
    let (proxy, server) = fidl::endpoints::create_proxy::<fi2c::DeviceMarker>()?;
    let () = root
        .open(
            fio::OpenFlags::empty(),
            fio::ModeType::empty(),
            device_path,
            fidl::endpoints::ServerEnd::new(server.into_channel()),
        )
        .context("Failed to open I2C device file")?;
    Ok(proxy)
}

async fn read_byte_from_i2c_device(device: &fi2c::DeviceProxy, address: &[u8]) -> Result<u8> {
    let transactions = &[
        fi2c::Transaction {
            data_transfer: Some(fi2c::DataTransfer::WriteData(address.to_owned())),
            ..Default::default()
        },
        fi2c::Transaction {
            data_transfer: Some(fi2c::DataTransfer::ReadSize(1)),
            stop: Some(true),
            ..Default::default()
        },
    ];
    let data = device
        .transfer(transactions)
        .await
        .context("Failed to send request to transfer transactions to I2C device")?
        .map_err(|status| zx::Status::from_raw(status))
        .context("Failed to transfer transactions to I2C device")?;
    if data.len() != 1 && data[0].len() != 1 {
        anyhow::bail!("Data size returned by I2C device is incorrect");
    }
    Ok(data[0][0])
}
