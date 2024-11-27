// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;

use anyhow::{Context, Result};
use args::ListCommand;
use bind::debugger::debug_dump::dump_bind_rules;
use fidl_fuchsia_driver_development as fdd;
use futures::join;
use std::collections::{HashMap, HashSet};
use std::io::Write;

pub async fn list(
    cmd: ListCommand,
    writer: &mut dyn Write,
    driver_development_proxy: fdd::ManagerProxy,
) -> Result<()> {
    let empty: [String; 0] = [];
    let driver_info = fuchsia_driver_dev::get_driver_info(&driver_development_proxy, &empty);

    let driver_info = if cmd.loaded {
        // Query devices and create a hash set of loaded drivers.
        let device_info = fuchsia_driver_dev::get_device_info(
            &driver_development_proxy,
            &empty,
            /* exact_match= */ false,
        );

        // Await the futures concurrently.
        let (driver_info, device_info) = join!(driver_info, device_info);

        let loaded_driver_set: HashSet<String> = HashSet::from_iter(
            device_info?.into_iter().filter_map(|device_info| device_info.bound_driver_url),
        );

        // Filter the driver list by the hash set.
        driver_info?
            .into_iter()
            .filter(|driver| {
                let mut loaded = false;
                if let Some(ref url) = driver.url {
                    if loaded_driver_set.contains(url) {
                        loaded = true
                    }
                }
                loaded
            })
            .collect()
    } else {
        driver_info.await.context("Failed to get driver info")?
    };

    if cmd.verbose {
        // Query devices and create a map of drivers to devices.
        let device_info = fuchsia_driver_dev::get_device_info(
            &driver_development_proxy,
            &empty,
            /* exact_match= */ false,
        )
        .await?;

        let mut driver_to_devices = HashMap::<String, Vec<String>>::new();
        for device in device_info.into_iter() {
            let driver = device.bound_driver_url.clone();
            let device_name = device.moniker.as_ref();
            if let (Some(driver), Some(device_name)) = (driver, device_name) {
                driver_to_devices
                    .entry(driver.to_string())
                    .and_modify(|v| v.push(device_name.to_string()))
                    .or_insert_with(|| vec![device_name.to_string()]);
            }
        }
        let driver_to_devices = driver_to_devices;

        for driver in driver_info {
            if let Some(name) = driver.name {
                writeln!(writer, "{0: <10}: {1}", "Name", name)?;
            }
            if let Some(url) = driver.url.as_ref() {
                writeln!(writer, "{0: <10}: {1}", "URL", url)?;
            }

            // If the version isn't set, the value is assumed to be 1.
            writeln!(
                writer,
                "{0: <10}: {1}",
                "DF Version",
                driver.driver_framework_version.unwrap_or(1)
            )?;

            if let Some(device_categories) = driver.device_categories {
                write!(writer, "Device Categories: [")?;

                for (i, category_table) in device_categories.iter().enumerate() {
                    if let Some(category) = &category_table.category {
                        if let Some(subcategory) = &category_table.subcategory {
                            if !subcategory.is_empty() {
                                write!(writer, "{}::{}", category, subcategory)?;
                            } else {
                                write!(writer, "{}", category,)?;
                            }
                        } else {
                            write!(writer, "{}", category,)?;
                        }
                    }

                    if i != device_categories.len() - 1 {
                        write!(writer, ", ")?;
                    }
                }
                writeln!(writer, "]")?;
            }

            if let Some(url) = driver.url {
                if let Some(devices) = driver_to_devices.get(&url) {
                    writeln!(writer, "{0: <10}:\n  {1}", "Devices", devices.join("\n  "))?;
                }
            }

            // TODO(b/303084353): Once the merkle_root is available, write it here.
            let bind_rules =
                driver.bind_rules_bytecode.map(|bytecode| dump_bind_rules(bytecode).ok()).flatten();
            match bind_rules {
                Some(bind_rules) => {
                    writeln!(writer, "{0: <10}: ", "Bind rules bytecode")?;
                    writeln!(writer, "{}", bind_rules)?;
                }
                _ => writeln!(writer, "Issue parsing the bind rules bytecode")?,
            }
            writeln!(writer)?;
        }
    } else {
        for driver in driver_info {
            if let Some(name) = driver.name {
                let url = driver.url.unwrap_or_default();
                writeln!(writer, "{:<20}: {}", name, url)?;
            } else {
                let url = driver.url.unwrap_or_default();
                writeln!(writer, "{}", url)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argh::FromArgs;
    use fidl::endpoints::ServerEnd;
    use futures::future::{Future, FutureExt};
    use futures::stream::StreamExt;
    use {fidl_fuchsia_driver_framework as fdf, fuchsia_async as fasync};

    /// Invokes `list` with `cmd` and runs a mock driver development server that
    /// invokes `on_driver_development_request` whenever it receives a request.
    /// The output of `list` that is normally written to its `writer` parameter
    /// is returned.
    async fn test_list<F, Fut>(cmd: ListCommand, on_driver_development_request: F) -> Result<String>
    where
        F: Fn(fdd::ManagerRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + Sync,
    {
        let (driver_development_proxy, mut driver_development_requests) =
            fidl::endpoints::create_proxy_and_stream::<fdd::ManagerMarker>()
                .context("Failed to create FIDL proxy")?;

        // Run the command and mock driver development server.
        let mut writer = Vec::new();
        let request_handler_task = fasync::Task::spawn(async move {
            while let Some(res) = driver_development_requests.next().await {
                let request = res.context("Failed to get next request")?;
                on_driver_development_request(request).await.context("Failed to handle request")?;
            }
            anyhow::bail!("Driver development request stream unexpectedly closed");
        });
        futures::select! {
            res = request_handler_task.fuse() => {
                res?;
                anyhow::bail!("Request handler task unexpectedly finished");
            }
            res = list(cmd, &mut writer, driver_development_proxy).fuse() => res.context("List command failed")?,
        }

        String::from_utf8(writer).context("Failed to convert list output to a string")
    }

    async fn run_driver_info_iterator_server(
        mut driver_infos: Vec<fdf::DriverInfo>,
        iterator: ServerEnd<fdd::DriverInfoIteratorMarker>,
    ) -> Result<()> {
        let mut iterator = iterator.into_stream();
        while let Some(res) = iterator.next().await {
            let request = res.context("Failed to get request")?;
            match request {
                fdd::DriverInfoIteratorRequest::GetNext { responder } => {
                    responder
                        .send(&driver_infos)
                        .context("Failed to send driver infos to responder")?;
                    driver_infos.clear();
                }
            }
        }
        Ok(())
    }

    async fn run_device_info_iterator_server(
        mut device_infos: Vec<fdd::NodeInfo>,
        iterator: ServerEnd<fdd::NodeInfoIteratorMarker>,
    ) -> Result<()> {
        let mut iterator = iterator.into_stream();
        while let Some(res) = iterator.next().await {
            let request = res.context("Failed to get request")?;
            match request {
                fdd::NodeInfoIteratorRequest::GetNext { responder } => {
                    responder
                        .send(&device_infos)
                        .context("Failed to send device infos to responder")?;
                    device_infos.clear();
                }
            }
        }
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_verbose() {
        let cmd = ListCommand::from_args(&["list"], &["--verbose"]).unwrap();

        let output = test_list(cmd, |request: fdd::ManagerRequest| async move {
            match request {
                fdd::ManagerRequest::GetDriverInfo {
                    driver_filter: _,
                    iterator,
                    control_handle: _,
                } => run_driver_info_iterator_server(
                    vec![fdf::DriverInfo {
                        name: Some("foo".to_owned()),
                        url: Some("fuchsia-pkg://fuchsia.com/foo-package#meta/foo.cm".to_owned()),
                        package_type: Some(fdf::DriverPackageType::Base),
                        device_categories: Some(vec![
                            fdf::DeviceCategory {
                                category: Some("connectivity".to_string()),
                                subcategory: Some("ethernet".to_string()),
                                ..Default::default()
                            },
                            fdf::DeviceCategory {
                                category: Some("usb".to_string()),
                                subcategory: None,
                                ..Default::default()
                            },
                        ]),
                        ..Default::default()
                    }],
                    iterator,
                )
                .await
                .context("Failed to run driver info iterator server")?,
                fdd::ManagerRequest::GetNodeInfo {
                    node_filter: _,
                    iterator,
                    control_handle: _,
                    exact_match: _,
                } => run_device_info_iterator_server(
                    vec![fdd::NodeInfo {
                        bound_driver_url: Some(
                            "fuchsia-pkg://fuchsia.com/foo-package#meta/foo.cm".to_owned(),
                        ),
                        moniker: Some("dev.sys.foo".to_owned()),
                        ..Default::default()
                    }],
                    iterator,
                )
                .await
                .context("Failed to run device info iterator server")?,
                _ => {}
            }
            Ok(())
        })
        .await
        .unwrap();

        println!("{}", output);

        assert_eq!(
            output,
            r#"Name      : foo
URL       : fuchsia-pkg://fuchsia.com/foo-package#meta/foo.cm
DF Version: 1
Device Categories: [connectivity::ethernet, usb]
Devices   :
  dev.sys.foo
Issue parsing the bind rules bytecode

"#
        );
    }
}
