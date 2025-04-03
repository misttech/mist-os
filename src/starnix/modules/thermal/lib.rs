// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::Proxy;
use fidl_fuchsia_hardware_temperature as ftemperature;
use fuchsia_async::TimeoutExt;
use fuchsia_component::client::connect_to_named_protocol_at_dir_root;
use futures::TryStreamExt;
use once_cell::sync::OnceCell;
use starnix_core::device::kobject::Device;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    fs_node_impl_dir_readonly, BytesFile, BytesFileOps, DirectoryEntryType, FileOps, FsNode,
    FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, VecDirectory, VecDirectoryEntry,
};
use starnix_logging::log_error;
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use zx::MonotonicInstant;

const TEMPERATURE_DRIVER_DIR: &str = "/dev/class/trippoint";

struct ThermalZoneDirectory {
    base_dir: DeviceDirectory,
    device_type: String,
    proxy: Arc<OnceCell<ftemperature::DeviceSynchronousProxy>>,
}

impl ThermalZoneDirectory {
    fn new(
        device: Device,
        proxy: Arc<OnceCell<ftemperature::DeviceSynchronousProxy>>,
        device_type: String,
    ) -> Self {
        let base_dir = DeviceDirectory::new(device);
        Self { device_type, proxy, base_dir }
    }
}

impl FsNodeOps for ThermalZoneDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let mut entries = self.base_dir.create_file_ops_entries();
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"temp".into(),
            inode: None,
        });
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"type".into(),
            inode: None,
        });
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"policy".into(),
            inode: None,
        });
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"available_policies".into(),
            inode: None,
        });
        Ok(VecDirectory::new_file(entries))
    }

    fn lookup(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match &**name {
            b"temp" => Ok(node.fs().create_node(
                current_task,
                TemperatureFile::new_node(self.proxy.clone()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o664), FsCred::root()),
            )),
            b"type" => Ok(node.fs().create_node(
                current_task,
                BytesFile::new_node(format!("{}\n", self.device_type).into_bytes()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            b"policy" => Ok(node.fs().create_node(
                current_task,
                BytesFile::new_node(format!("{}\n", "step_wise").into_bytes()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            b"available_policies" => Ok(node.fs().create_node(
                current_task,
                BytesFile::new_node(format!("{}\n", "step_wise").into_bytes()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            _ => self.base_dir.lookup(locked, node, current_task, name),
        }
    }
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

pub fn thermal_device_init(
    locked: &mut Locked<'_, Unlocked>,
    system_task: &CurrentTask,
    devices: Vec<String>,
) {
    let kernel = system_task.kernel();
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
            system_task,
            thermal_zone.clone().as_str().into(),
            virtual_thermal_class.clone(),
            move |dev| ThermalZoneDirectory::new(dev, proxy.clone(), sensor_name.clone().into()),
        );

        sensor_proxies.insert(sensor_name_clone, proxy_clone);
    }

    kernel.kthreads.spawn_future(async move {
        let dir = fuchsia_fs::directory::open_in_namespace(
            TEMPERATURE_DRIVER_DIR,
            fuchsia_fs::PERM_READABLE,
        )
        .expect("Failed to open temperature driver dir");

        let mut watcher =
            fuchsia_fs::directory::Watcher::new(&dir).await.expect("Failed to create watcher");

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
