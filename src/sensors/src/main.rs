// Copyright 2023 The Fuchsia Authors.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{anyhow, Error};
use fidl_fuchsia_hardware_sensors as driver_fidl;
use sensors_lib::sensor_manager::{Playback, SensorManager};

async fn connect_to_playback() -> Option<(driver_fidl::DriverProxy, driver_fidl::PlaybackProxy)> {
    let playback_proxy_res =
        fuchsia_component::client::connect_to_protocol::<driver_fidl::PlaybackMarker>();

    let playback_proxy = if let Ok(playback_proxy) = playback_proxy_res {
        playback_proxy
    } else {
        tracing::warn!(
            "Failed to connect to sensor playback driver protocol. {:#?}",
            playback_proxy_res
        );
        return None;
    };

    let mut playback_driver_proxy: Option<driver_fidl::DriverProxy> = None;

    // Attempt to open and connect to the playback service exposed by the subpackaged
    // sensors_playback. This service is exposed at /svc/fuchsia.hardware.sensors.Service.Playback to
    // differentiate it from the real services.
    //
    // TODO(b/370821933): Instead of connecting to the playback service before creating the sensor
    // manager, we should wait for playback to be configured so that playback is treated as a
    // normal driver.
    if let Ok(playback_exposed_dir) =
        fuchsia_component::client::open_childs_exposed_directory("sensors_playback", None).await
    {
        let driver_service_dir_res = fuchsia_component::client::open_service_at_dir::<
            driver_fidl::ServiceMarker,
        >(&playback_exposed_dir);

        if let Ok(driver_service_dir) = driver_service_dir_res {
            if let Ok(instances) = fuchsia_fs::directory::readdir(&driver_service_dir).await {
                if let Some(instance) = instances.first() {
                    playback_driver_proxy =
                        connect_to_instance(Some(driver_service_dir), &instance.name);
                }
            }
        }
    }

    if let Some(driver_proxy) = playback_driver_proxy {
        Some((driver_proxy, playback_proxy))
    } else {
        tracing::warn!("Failed to connect to sensor playback driver service.");
        None
    }
}

async fn connect_to_sensors_service() -> Vec<driver_fidl::DriverProxy> {
    let mut drivers = Vec::<driver_fidl::DriverProxy>::new();

    // Attempt to open the sensors service. If no sensor drivers are present in the system,
    // the sensors framework will attempt to use the playback system. If neither are available, the
    // sensors framework will error as there are no sensors connected.
    //
    // TODO(b/370822675): Devices may not have exposed their services before the sensor manager has
    // started. Consider watching this directory and connecting to the drivers as new instances are
    // added.
    if let Ok(dir_proxy) = fuchsia_component::client::open_service::<driver_fidl::ServiceMarker>() {
        if let Ok(instances) = fuchsia_fs::directory::readdir(&dir_proxy).await {
            let instances = instances.into_iter().map(|dirent| dirent.name);

            for instance in instances {
                if let Some(driver_proxy) = connect_to_instance(None, &instance) {
                    drivers.push(driver_proxy);
                } else {
                    tracing::warn!(
                        "Failed to connect to driver in service instance! Instance name: {:#?}",
                        instance
                    );
                }
            }
        } else {
            tracing::warn!("Failed to read service instance entries!");
        }
    } else {
        tracing::warn!("Failed to open sensors service directory.");
    }

    drivers
}

fn connect_to_instance(
    directory: Option<fidl_fuchsia_io::DirectoryProxy>,
    instance: &String,
) -> Option<driver_fidl::DriverProxy> {
    // The playback service is exposed in a separate directory to differentiate it because it does
    // not come from the same collection.
    let service_proxy = if let Some(dir) = directory {
        fuchsia_component::client::connect_to_instance_in_service_dir::<driver_fidl::ServiceMarker>(
            &dir, &instance,
        )
    } else {
        fuchsia_component::client::connect_to_service_instance::<driver_fidl::ServiceMarker>(
            &instance,
        )
    };

    if let Ok(service_proxy) = service_proxy {
        if let Ok(driver) = service_proxy.connect_to_driver() {
            return Some(driver);
        } else {
            tracing::error!("service proxy failed to connect to driver");
            return None;
        }
    } else {
        return None;
    }
}

#[fuchsia::main(logging_tags = [ "sensors" ])]
async fn main() -> Result<(), Error> {
    tracing::info!("Sensors Server Started");

    let playback_proxies = connect_to_playback().await;
    let drivers = connect_to_sensors_service().await;

    if drivers.is_empty() {
        if playback_proxies.is_some() {
            tracing::info!("No sensor drivers were found. Starting with playback sensors only.");
        } else {
            return Err(anyhow!(
                "Failed to open sensors service and sensor playback is not enabled on the system"
            ));
        }
    }

    let mut playback: Option<Playback> = None;
    if let Some((driver_proxy, playback_proxy)) = playback_proxies {
        playback = Some(Playback::new(driver_proxy, playback_proxy));
    }

    let mut sensor_manager = SensorManager::new(drivers, playback);

    // This should run forever.
    let result = sensor_manager.run().await;
    tracing::error!("Unexpected exit with result: {:?}", result);
    result
}
