// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "256"]

use fidl::endpoints::Proxy;
use fidl_fuchsia_hardware_temperature as ftemperature;
use fuchsia_async::TimeoutExt;
use fuchsia_component::client::connect_to_named_protocol_at_dir_root;
use futures::TryStreamExt;
use once_cell::sync::OnceCell;
use starnix_core::device::kobject::Device;
use starnix_core::fs::sysfs::build_device_directory;
use starnix_core::task::{CurrentTask, Kernel};
use starnix_core::vfs::pseudo::simple_directory::SimpleDirectoryMutator;
use starnix_core::vfs::pseudo::simple_file::{BytesFile, BytesFileOps};
use starnix_core::vfs::FsNodeOps;
use starnix_logging::{log_error, log_warn};
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use zx::MonotonicInstant;

const TEMPERATURE_DRIVER_DIR: &str = "/dev/class/trippoint";

fn build_thermal_zone_directory(
    device: &Device,
    proxy: Arc<OnceCell<ftemperature::DeviceSynchronousProxy>>,
    device_type: String,
    dir: &SimpleDirectoryMutator,
) {
    build_device_directory(device, dir);
    dir.entry("temp", TemperatureFile::new_node(proxy), mode!(IFREG, 0o664));
    dir.entry(
        "type",
        BytesFile::new_node(format!("{}\n", device_type).into_bytes()),
        mode!(IFREG, 0o444),
    );
    dir.entry("policy", BytesFile::new_node(b"step_wise\n".to_vec()), mode!(IFREG, 0o444));
    dir.entry(
        "available_policies",
        BytesFile::new_node(b"step_wise\n".to_vec()),
        mode!(IFREG, 0o444),
    );
}

struct TemperatureFile {
    proxy: Arc<OnceCell<ftemperature::DeviceSynchronousProxy>>,
}

impl TemperatureFile {
    pub fn new_node(proxy: Arc<OnceCell<ftemperature::DeviceSynchronousProxy>>) -> impl FsNodeOps {
        BytesFile::new_node(Self { proxy })
    }
}

impl BytesFileOps for TemperatureFile {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let (zx_status, temp) =
            self.proxy.wait().get_temperature_celsius(MonotonicInstant::INFINITE).map_err(|e| {
                log_error!("get_temperature_celsius failed: {}", e);
                Errno::new(starnix_uapi::errors::ENOENT)
            })?;
        let _ = zx::Status::ok(zx_status).map_err(|e| {
            log_error!("get_temperature_celsius driver returned error: {}", e);
            Errno::new(starnix_uapi::errors::ENOENT)
        })?;

        let out = format!("{}\n", (temp * 1000.0) as i32);
        Ok(out.as_bytes().to_owned().into())
    }
}

pub fn thermal_device_init(locked: &mut Locked<Unlocked>, kernel: &Kernel, devices: Vec<String>) {
    let registry = &kernel.device_registry;
    let virtual_thermal_class = registry.objects.virtual_thermal_class();

    let mut sensor_proxies = HashMap::new();

    for (index, sensor_name) in devices.into_iter().enumerate() {
        let thermal_zone = format!("thermal_zone{}", index);
        let proxy = Arc::new(OnceCell::new());
        let proxy_clone = proxy.clone();
        let sensor_name_clone = sensor_name.clone();

        registry.add_numberless_device(
            locked,
            thermal_zone.clone().as_str().into(),
            virtual_thermal_class.clone(),
            |device, dir| build_thermal_zone_directory(device, proxy, sensor_name, dir),
        );

        sensor_proxies.insert(sensor_name_clone, proxy_clone);
    }

    kernel.kthreads.spawn_future(async move {
        // TODO: Move this to expect once test support is enabled
        let dir = match fuchsia_fs::directory::open_in_namespace(
            TEMPERATURE_DRIVER_DIR,
            fuchsia_fs::PERM_READABLE,
        ) {
            Ok(dir) => dir,
            Err(e) => {
                log_warn!("Failed to open temperature driver directory: {:}", e);
                return;
            }
        };

        let mut watcher = match fuchsia_fs::directory::Watcher::new(&dir).await {
            Ok(watcher) => watcher,
            Err(e) => {
                log_warn!("Failed to create directory watcher for temperature device: {:}", e);
                return;
            }
        };

        while !sensor_proxies.is_empty() {
            if let Ok(Some(watch_msg)) = watcher
                .try_next()
                .on_timeout(MonotonicInstant::INFINITE, || Ok(None))
                .await
                .map_err(|e| {
                    log_error!("Error from temperature driver: {:?}", e);
                })
            {
                let filename = watch_msg
                    .filename
                    .as_path()
                    .to_str()
                    .expect("Failed to convert watch_msg to str");
                if filename != "." {
                    if watch_msg.event == fuchsia_fs::directory::WatchEvent::ADD_FILE
                        || watch_msg.event == fuchsia_fs::directory::WatchEvent::EXISTING
                    {
                        let async_proxy = connect_to_named_protocol_at_dir_root::<
                            ftemperature::DeviceMarker,
                        >(&dir, &filename)
                        .expect("connect_to_named_protocol_at_dir_root failed");

                        let name =
                            async_proxy.get_sensor_name().await.expect("Failed to get sensor name");

                        if let Some(proxy) = sensor_proxies.remove(&name) {
                            let _ = proxy
                                .set(
                                    async_proxy
                                        .into_client_end()
                                        .expect("Failed to conver proxy to client end")
                                        .into_sync_proxy(),
                                )
                                .expect("Proxy already set");
                        }
                    }
                }
            }
        }
    });
}
