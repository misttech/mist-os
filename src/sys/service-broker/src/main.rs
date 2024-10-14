// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context, Result};
use fidl_fuchsia_io as fio;
use fuchsia_component::server::{ServiceFs, ServiceObjLocal, ServiceObjTrait};
use fuchsia_fs::directory::{open_in_namespace_deprecated, WatchEvent, Watcher};
use futures::prelude::*;
use service_broker_config::Config;

async fn wait_for_first_instance() -> Result<String> {
    const INPUT_SERVICE: &str = "/svc/input";
    let service_dir = open_in_namespace_deprecated(
        INPUT_SERVICE,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
    )?;
    let watcher = Watcher::new(&service_dir).await.context("failed to create watcher")?;
    let mut stream =
        watcher.map(|result| result.context("failed to get watcher event")).try_filter_map(|msg| {
            futures::future::ok(match msg.event {
                WatchEvent::EXISTING | WatchEvent::ADD_FILE => {
                    if msg.filename == std::path::Path::new(".") {
                        None
                    } else {
                        Some(msg.filename)
                    }
                }
                _ => None,
            })
        });
    let first = stream.try_next().await?.unwrap();
    let filename = first.to_str().ok_or(format_err!("to_str for filename failed"))?;
    Ok(format!("{INPUT_SERVICE}/{filename}"))
}

async fn first_instance_to_protocol<'a>(
    fs: &mut ServiceFs<ServiceObjLocal<'a, ()>>,
    protocol_name: &'a str,
) -> Result<()> {
    if protocol_name == "" {
        return Err(format_err!("Invalid protocol name provided"));
    }

    // TODO(surajmalhotra): Do this wait every time we get a connection request to handle cases
    // where the instance goes away and comes back.
    let instance_dir = wait_for_first_instance().await?;

    fs.dir("svc").add_service_at("protocol", move |request: zx::Channel| {
        fdio::service_connect(&format!("{instance_dir}/{protocol_name}"), request)
            .expect("service connect failed");
        Some(())
    });

    Ok(())
}

async fn first_instance_to_default<T: ServiceObjTrait>(fs: &mut ServiceFs<T>) -> Result<()> {
    // TODO(surajmalhotra): Do this wait every time we get a connection request to handle cases
    // where the instance goes away and comes back.
    let instance_dir = wait_for_first_instance().await?;
    let instance_dir = open_in_namespace_deprecated(
        &instance_dir,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
    )?;

    fs.dir("svc").dir("service").add_remote("default", instance_dir);
    Ok(())
}

async fn filter_and_rename<T: ServiceObjTrait>(
    _fs: &mut ServiceFs<T>,
    _filter: &Vec<String>,
    _rename: &Vec<String>,
) -> Result<()> {
    unimplemented!();
    // Add a bunch of directories which forward requests?
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<()> {
    let config = Config::take_from_startup_handle();
    let mut fs = ServiceFs::new_local();
    match config.policy.as_str() {
        "first_instance_to_protocol" => {
            first_instance_to_protocol(&mut fs, &config.protocol_name).await
        }
        "first_instance_to_default" => first_instance_to_default(&mut fs).await,
        "filter_and_rename" => filter_and_rename(&mut fs, &config.filter, &config.rename).await,
        _ => Err(format_err!("Unsupported policy specified: {}", config.policy)),
    }?;

    tracing::debug!("Initialized.");

    fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;
    fs.collect::<()>().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[fuchsia::test]
    async fn smoke_test() {
        assert!(true);
    }
}
