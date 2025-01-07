// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, format_err, Context, Result};
use fidl::endpoints::{Proxy, ServerEnd};
use fidl::HandleBased;
use fuchsia_component::directory::AsRefDirectory;
use fuchsia_component::server::{ServiceFs, ServiceObj, ServiceObjTrait};
use fuchsia_fs::directory::{WatchEvent, Watcher};
use futures::prelude::*;
use {
    fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio, fidl_fuchsia_process as fprocess,
    fidl_fuchsia_process_lifecycle as fpl,
};

async fn wait_for_first_instance(svc: &fio::DirectoryProxy) -> Result<String> {
    const INPUT_SERVICE: &str = "input";
    let (service_dir, request) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    svc.as_ref_directory().open(INPUT_SERVICE, fio::Flags::PROTOCOL_DIRECTORY, request.into())?;
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
    let filename = first.to_str().ok_or_else(|| format_err!("to_str for filename failed"))?;
    Ok(format!("{INPUT_SERVICE}/{filename}"))
}

async fn first_instance_to_protocol<'a>(
    svc: fio::DirectoryProxy,
    fs: &mut ServiceFs<ServiceObj<'a, ()>>,
    protocol_name: &str,
) -> Result<()> {
    if protocol_name == "" {
        bail!("Invalid protocol name provided");
    }

    // TODO(surajmalhotra): Do this wait every time we get a connection request to handle cases
    // where the instance goes away and comes back.
    let instance_dir = wait_for_first_instance(&svc).await?;

    let svc = svc.into_channel().unwrap().into_zx_channel();
    let protocol_name = protocol_name.to_string();
    fs.dir("svc").add_service_at("output", move |request: zx::Channel| {
        if let Err(_) =
            fdio::service_connect_at(&svc, &format!("{instance_dir}/{protocol_name}"), request)
        {
            log::error!(
                "[service-broker] Failed to forward connection to {instance_dir}/{protocol_name}"
            );
        }
        Some(())
    });

    Ok(())
}

async fn first_instance_to_default<T: ServiceObjTrait>(
    svc: fio::DirectoryProxy,
    fs: &mut ServiceFs<T>,
) -> Result<()> {
    // TODO(surajmalhotra): Do this wait every time we get a connection request to handle cases
    // where the instance goes away and comes back.
    let instance_dir_path = wait_for_first_instance(&svc).await?;
    let (instance_dir, request) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    svc.as_ref_directory().open(
        &instance_dir_path,
        fio::Flags::PROTOCOL_DIRECTORY,
        request.into(),
    )?;

    fs.dir("svc").dir("output").add_remote("default", instance_dir);
    Ok(())
}

async fn filter_and_rename<T: ServiceObjTrait>(
    _svc: fio::DirectoryProxy,
    _fs: &mut ServiceFs<T>,
    _filter: &Vec<String>,
    _rename: &Vec<String>,
) -> Result<()> {
    unimplemented!();
    // Add a bunch of directories which forward requests?
}

fn get_value<'a>(dict: &'a fdata::Dictionary, key: &str) -> Option<&'a fdata::DictionaryValue> {
    match &dict.entries {
        Some(entries) => {
            for entry in entries {
                if entry.key == key {
                    return entry.value.as_ref().map(|val| &**val);
                }
            }
            None
        }
        _ => None,
    }
}

fn get_program_string<'a>(program: &'a fdata::Dictionary, key: &str) -> Result<&'a str> {
    if let Some(fdata::DictionaryValue::Str(value)) = get_value(program, key) {
        Ok(value)
    } else {
        Err(format_err!("{key} not found in program or is not a string"))
    }
}

fn get_program_strvec<'a>(
    program: &'a fdata::Dictionary,
    key: &str,
) -> Result<Option<&'a Vec<String>>> {
    match get_value(program, key) {
        Some(args_value) => match args_value {
            fdata::DictionaryValue::StrVec(vec) => Ok(Some(vec)),
            _ => Err(format_err!(
                "Expected {key} in program to be vector of strings, found something else"
            )),
        },
        None => Ok(None),
    }
}

pub async fn main(
    ns_entries: Vec<fprocess::NameInfo>,
    directory_request: ServerEnd<fio::DirectoryMarker>,
    _lifecycle: ServerEnd<fpl::LifecycleMarker>,
    program: Option<fdata::Dictionary>,
) -> Result<()> {
    if directory_request.is_invalid_handle() {
        bail!("No valid handle found for outgoing directory");
    }
    let Some(svc) = ns_entries.into_iter().find(|e| e.path == "/svc") else {
        bail!("No /svc in namespace");
    };
    let Some(program) = program else {
        bail!("No program section provided");
    };
    let svc = svc.directory.into_proxy();
    let mut fs = ServiceFs::new();
    match get_program_string(&program, "policy")? {
        "first_instance_to_protocol" => {
            let protocol_name = get_program_string(&program, "protocol_name")?;
            first_instance_to_protocol(svc, &mut fs, protocol_name).await
        }
        "first_instance_to_default" => first_instance_to_default(svc, &mut fs).await,
        "filter_and_rename" => {
            let empty = vec![];
            let filter = get_program_strvec(&program, "filter")?.unwrap_or(&empty);
            let rename = get_program_strvec(&program, "rename")?.unwrap_or(&empty);
            filter_and_rename(svc, &mut fs, filter, rename).await
        }
        policy => Err(format_err!("Unsupported policy specified: {policy}")),
    }?;

    log::debug!("[service-broker] Initialized.");

    fs.serve_connection(directory_request).context("failed to serve outgoing namespace")?;
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
