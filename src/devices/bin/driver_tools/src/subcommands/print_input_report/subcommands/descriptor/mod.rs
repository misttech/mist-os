// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use anyhow::{Context, Result};
use args::DescriptorCommand;
use fuchsia_async::Task;
use futures::lock::Mutex;
use futures::stream::TryStreamExt;
use std::fmt::Debug;
use std::io::Write;
use std::ops::DerefMut;
use std::path::Path;
use std::sync::Arc;
use {fidl_fuchsia_input_report as fir, fidl_fuchsia_io as fio};

async fn get_and_write_descriptor(
    input_device_proxy: fir::InputDeviceProxy,
    input_device_path: impl AsRef<Path> + Debug,
    writer: Arc<Mutex<impl Write + Send + Sync + 'static>>,
) -> Result<()> {
    let descriptor = input_device_proxy
        .get_descriptor()
        .await
        .context("Failed to send request to get descriptor")?;
    let mut writer = writer.lock().await;
    writeln!(&mut writer, "Descriptor from file: {:?}", input_device_path.as_ref(),)?;
    super::write_descriptor(writer.deref_mut(), &descriptor)
        .context("Failed to write input descriptor")?;
    Ok(())
}

pub async fn descriptor(
    cmd: &DescriptorCommand,
    writer: Arc<Mutex<impl Write + Send + Sync + 'static>>,
    dev: fio::DirectoryProxy,
) -> Result<()> {
    if let Some(ref device_path) = cmd.device_path {
        let input_device_proxy = super::connect_to_input_device(&dev, device_path)
            .with_context(|| format!("Failed to get input device proxy from {:?}", device_path))?;
        get_and_write_descriptor(input_device_proxy, device_path, writer).await?;
        return Ok(());
    }
    let input_device_paths = super::get_all_input_device_paths(&dev)
        .await
        .context("Failed to get all input device paths")?;
    futures::pin_mut!(input_device_paths);
    while let Some(input_device_path) =
        input_device_paths.try_next().await.context("Failed to watch directory")?
    {
        let writer = Arc::clone(&writer);
        let input_device_proxy = super::connect_to_input_device(&dev, &input_device_path)
            .with_context(|| {
                format!("Failed to get input device proxy from {:?}", &input_device_path)
            })?;
        Task::spawn(async move {
            if let Err(err) =
                get_and_write_descriptor(input_device_proxy, &input_device_path, writer).await
            {
                log::error!(
                    from:? = input_device_path, err:%; "Failed to print input descriptors");
            }
        })
        .detach();
    }
    Ok(())
}
