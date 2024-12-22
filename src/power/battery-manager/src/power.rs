// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Error};
use futures::prelude::*;
use log::{debug, error, info, warn};
use std::path::PathBuf;
use std::sync::Arc;
use zx::{self as zx, Signals};
use {fidl_fuchsia_hardware_powersource as hpower, fuchsia_async as fasync};

use crate::battery_manager::BatteryManager;

// TODO(https://fxbug.dev/42108351): binding the FIDL service via file descriptor is still
// required for hardware FIDLs (implemented by ACPI battery driver).
// Once componentization of drivers is complete and they are capable of
// publishing their FIDL services, should be abl to remove POWER_DEVICE
// specifically and refactor the "power" module in general to leverage
// the discoverable service.
static POWER_DEVICE: &str = "/dev/class/power";

#[derive(Debug)]
enum WatchSuccess {
    Completed,
    BatteryAlreadyFound,
    AdapterAlreadyFound,
}

fn get_power_source_proxy(file: &PathBuf) -> Result<hpower::SourceProxy, Error> {
    fuchsia_component::client::connect_to_protocol_at_path::<hpower::SourceMarker>(
        file.as_path().to_str().unwrap(),
    )
}

// Get the power info from file descriptor/hardware.power FIDL service
// Note that this file (/dev/class/power) is a left over artifact of the
// legacy power_manager implementation which was based on power IOCTLs.
// The file is still required as it provides the descriptor with which
// to bind the FIDL service, at least until https://fxbug.dev/42108351 is complete, which
// will componentize drivers and allow them to provide discoverable FIDL
// services like everyone else.
pub async fn get_power_info(file: &PathBuf) -> Result<hpower::SourceInfo, Error> {
    let power_source = get_power_source_proxy(&file)?;
    load_power_info(&power_source).await
}

pub async fn load_power_info(
    power_source: &hpower::SourceProxy,
) -> Result<hpower::SourceInfo, Error> {
    match power_source.get_power_info().map_err(|_| zx::Status::IO).await? {
        result => {
            let (status, info) = result;
            debug!(get_power_info:? = info, status:?; "::power::");
            Ok(info)
        }
    }
}

// Get the battery info from file descriptor/hardware.power FIDL service
// Note that this file (/dev/class/power) is a left over artifact of the
// legacy power_manager implementation which was based on power IOCTLs.
// The file is still required as it provides the descriptor with which
// to bind the FIDL service, at least until https://fxbug.dev/42108351 is complete, which
// will componentize drivers and allow them to provide discoverable FIDL
// services like everyone else.
pub async fn get_battery_info(file: &PathBuf) -> Result<hpower::BatteryInfo, Error> {
    let power_source = get_power_source_proxy(&file)?;
    load_battery_info(&power_source).await
}

pub async fn load_battery_info(
    power_source: &hpower::SourceProxy,
) -> Result<hpower::BatteryInfo, Error> {
    match power_source.get_battery_info().map_err(|_| zx::Status::IO).await? {
        result => {
            let (status, info) = result;
            debug!(get_battery_info:? = info, status:?; "::power::");
            Ok(info)
        }
    }
}

fn add_listener<F>(file: &PathBuf, callback: F) -> Result<(), Error>
where
    F: 'static + Send + Fn(hpower::SourceInfo, Option<hpower::BatteryInfo>) + Sync,
{
    let power_source = get_power_source_proxy(&file)?;
    debug!("::power:: spawn device state change event listener");
    fasync::Task::spawn(
        async move {
            let (_status, handle) =
                power_source.get_state_change_event().map_err(|_| zx::Status::IO).await?;

            loop {
                // Note that load_battery_info & wait on signal must
                // occur within the loop as it is the former call that
                // clears the signal bit following its setting during
                // the notification.
                debug!("::power event listener:: waiting on signal for state change event");
                fasync::OnSignals::new(&handle, Signals::USER_0).await?;
                debug!("::power event listener:: got signal for state change event");

                let power_info = load_power_info(&power_source).await?;
                let mut battery_info = None;
                if power_info.type_ == hpower::PowerType::Battery {
                    battery_info = Some(load_battery_info(&power_source).await?);
                }
                callback(power_info, battery_info);
            }
        }
        .unwrap_or_else(|e: anyhow::Error| {
            error!("not able to apply listener to power device, wait failed: {:?}", e)
        }),
    )
    .detach();

    Ok(())
}

async fn process_watch_event(
    filepath: &PathBuf,
    battery_manager: Arc<BatteryManager>,
    battery_device_found: &mut bool,
    adapter_device_found: &mut bool,
) -> Result<WatchSuccess, anyhow::Error> {
    debug!("::power:: process_watch_event for {:#?}", &filepath);

    let file = filepath.clone();
    let power_info = get_power_info(&file).await?;

    let mut battery_info = None;
    if power_info.type_ == hpower::PowerType::Battery {
        if *battery_device_found {
            return Ok(WatchSuccess::BatteryAlreadyFound);
        } else {
            battery_info = Some(get_battery_info(&file).await?);
        }
    } else if power_info.type_ == hpower::PowerType::Ac && *adapter_device_found {
        return Ok(WatchSuccess::AdapterAlreadyFound);
    }

    // add the listener to wait on the signal/notification from
    // state event change interface provided by the hardware FIDL
    let battery_manager2 = battery_manager.clone();
    debug!("::power:: process_watch_event add_listener with callback");
    add_listener(&file, move |p_info, b_info| {
        debug!("::power event listener:: callback firing => UPDATE_STATUS");
        let battery_manager2 = battery_manager2.clone();
        fasync::Task::spawn(async move {
            if let Err(err) = battery_manager2.update_status(p_info.clone(), b_info.clone()) {
                error!(err:%; "");
            }
        })
        .detach()
    })?;

    if power_info.type_ == hpower::PowerType::Battery {
        *battery_device_found = true;
        // poll and update battery status to catch changes that might not
        // otherwise be notified (i.e. gradual charge/discharge)
        let battery_manager = battery_manager.clone();
        let mut timer = fasync::Interval::new(zx::MonotonicDuration::from_seconds(60));
        debug!("::power:: process_watch_event spawn periodic timer");

        fasync::Task::spawn(async move {
            while let Some(()) = (timer.next()).await {
                debug!("::power:: periodic timer fired => UPDDATE_STATUS");
                let power_info = get_power_info(&file).await.unwrap();
                let battery_info = Some(get_battery_info(&file).await.unwrap());
                if let Err(err) =
                    battery_manager.update_status(power_info.clone(), battery_info.clone())
                {
                    error!(err:%; "");
                }
            }
        })
        .detach();
    } else {
        *adapter_device_found = true;
    }

    // update the status with the current state info from the watch event
    {
        debug!("::power:: process_watch_event => UPDATE_STATUS");
        battery_manager
            .update_status(power_info.clone(), battery_info)
            .context("adding watch events")?;
    }

    Ok(WatchSuccess::Completed)
}

pub async fn watch_power_device(battery_manager: Arc<BatteryManager>) -> Result<(), Error> {
    let dir_proxy =
        fuchsia_fs::directory::open_in_namespace(POWER_DEVICE, fuchsia_fs::PERM_READABLE)?;
    let mut stream = device_watcher::watch_for_files(&dir_proxy)
        .await
        .with_context(|| format!("Watching for files in {}", POWER_DEVICE))?;

    let mut adapter_device_found = false;
    let mut battery_device_found = false;

    while let Some(entry) =
        stream.try_next().await.with_context(|| format!("Getting a file from {}", POWER_DEVICE))?
    {
        debug!("::power:: watch_power_device trying next: {:#?}", &entry);
        if battery_device_found && adapter_device_found {
            continue;
        }
        let mut filepath = PathBuf::from(POWER_DEVICE);
        filepath.push(entry);
        info!("watch_power_device event for file: {:?}", &filepath);
        match process_watch_event(
            &filepath,
            battery_manager.clone(),
            &mut battery_device_found,
            &mut adapter_device_found,
        )
        .await
        {
            Ok(WatchSuccess::Completed) => {}
            Ok(early_return) => {
                let device_type = match early_return {
                    WatchSuccess::Completed => unreachable!(),
                    WatchSuccess::BatteryAlreadyFound => "battery",
                    WatchSuccess::AdapterAlreadyFound => "adapter",
                };
                debug!("::power:: Skip '{:?}' as {} device already found", filepath, device_type);
            }
            Err(err) => {
                warn!("Failed to add watch event for '{:?}': {}", filepath, err);
            }
        }
    }

    Ok(())
}
