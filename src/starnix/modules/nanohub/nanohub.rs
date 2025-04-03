// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::DerefMut;
use std::sync::Arc;

use crate::nanohub_comms_directory::NanohubCommsDirectory;
use crate::socket_tunnel_file::register_socket_tunnel_device;
use fidl_fuchsia_hardware_serial as fserial;
use futures::TryStreamExt;
use starnix_core::device::serial::SerialDevice;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::task::{CurrentTask, Kernel};
use starnix_core::vfs::{BytesFile, FsString, StaticDirectoryBuilder};
use starnix_logging::{log_error, log_info};
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::mode;

const SERIAL_DIRECTORY: &str = "/dev/class/serial";

/// Function to be invoked by ProcDirectory while constructing /proc/device-tree
pub fn nanohub_procfs_builder(
    builder: &'_ mut StaticDirectoryBuilder<'_>,
    current_task: &CurrentTask,
) {
    builder.subdir(current_task, "mcu", 0o555, |dir| {
        dir.entry(
            current_task,
            "board_type",
            BytesFile::new_node(b"starnix".to_vec()),
            mode!(IFREG, 0o444),
        );
    });
}

pub fn nanohub_device_init(locked: &mut Locked<'_, Unlocked>, current_task: &CurrentTask) {
    struct Descriptor {
        socket_label: FsString,
        dev_node_name: FsString,
    }

    let descriptors = vec![
        Descriptor { socket_label: b"/dev/nanohub".into(), dev_node_name: b"nanohub".into() },
        Descriptor {
            socket_label: b"/dev/nanohub_brightness".into(),
            dev_node_name: b"nanohub_brightness".into(),
        },
        Descriptor { socket_label: b"/dev/nanohub_bt".into(), dev_node_name: b"nanohub_bt".into() },
        Descriptor {
            socket_label: b"/dev/nanohub_console".into(),
            dev_node_name: b"nanohub_console".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_debug_log_data_link".into(),
            dev_node_name: b"nanohub_debug_log_data_link".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_debug_log".into(),
            dev_node_name: b"nanohub_debug_log".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_display".into(),
            dev_node_name: b"nanohub_display".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_metrics".into(),
            dev_node_name: b"nanohub_metrics".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_pele".into(),
            dev_node_name: b"nanohub_pele".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_render".into(),
            dev_node_name: b"nanohub_render".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_rpc0".into(),
            dev_node_name: b"nanohub_rpc0".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_rpc1".into(),
            dev_node_name: b"nanohub_rpc1".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_touch".into(),
            dev_node_name: b"nanohub_touch".into(),
        },
    ];

    for descriptor in descriptors {
        register_socket_tunnel_device(
            locked,
            current_task,
            descriptor.socket_label.as_ref(),
            descriptor.dev_node_name.as_ref(),
            b"nanohub".into(),
            DeviceDirectory::new,
        );
    }

    // /dev/nanohub_comms requires a set of additional sysfs nodes, so create this route
    // with a specialized NanohubCommsDirectory implementation.
    register_socket_tunnel_device(
        locked,
        current_task,
        "/dev/nanohub_comms".into(),
        "nanohub_comms".into(),
        "nanohub".into(),
        NanohubCommsDirectory::new,
    );

    // Spawn future to bind and configure serial device
    current_task.kernel().kthreads.spawn_future({
        let kernel = current_task.kernel().clone();
        async move { register_serial_device(kernel).await }
    });
}

async fn register_serial_device(kernel: Arc<Kernel>) {
    let current_task = kernel.kthreads.system_task();

    // TODO Move this to expect once test support is enabled
    let dir =
        match fuchsia_fs::directory::open_in_namespace(SERIAL_DIRECTORY, fuchsia_fs::PERM_READABLE)
        {
            Ok(dir) => dir,
            Err(e) => {
                log_error!("Failed to open serial directory: {:}", e);
                return;
            }
        };

    let mut watcher = match fuchsia_fs::directory::Watcher::new(&dir).await {
        Ok(watcher) => watcher,
        Err(e) => {
            log_info!("Failed to create directory watcher for serial device: {:}", e);
            return;
        }
    };

    loop {
        match watcher.try_next().await {
            Ok(Some(watch_msg)) => {
                let filename = watch_msg
                    .filename
                    .as_path()
                    .to_str()
                    .expect("Failed to convert watch_msg to str");
                if filename == "." {
                    continue;
                }
                if watch_msg.event == fuchsia_fs::directory::WatchEvent::ADD_FILE
                    || watch_msg.event == fuchsia_fs::directory::WatchEvent::EXISTING
                {
                    let instance_path = format!("{}/{}", SERIAL_DIRECTORY, filename);
                    let (client_channel, server_channel) = zx::Channel::create();
                    if let Err(_) = fdio::service_connect(&instance_path, server_channel) {
                        continue;
                    }

                    // `fuchsia.hardware.serial` exposes a `DeviceProxy` type used for binding with
                    // a `Device` type. This should not be confused with the `DeviceProxy` generated
                    // by FIDL
                    let device_proxy = fserial::DeviceProxy_SynchronousProxy::new(client_channel);
                    let (serial_proxy, server_end) =
                        fidl::endpoints::create_sync_proxy::<fserial::DeviceMarker>();

                    // Instruct the serial driver to bind the connection to the underlying device
                    if let Err(_) = device_proxy.get_channel(server_end) {
                        continue;
                    }

                    // Fetch the device class to see if this is the correct instance
                    let device_class = match serial_proxy.get_class(zx::MonotonicInstant::INFINITE)
                    {
                        Ok(class) => class,
                        Err(_) => continue,
                    };

                    if device_class == fserial::Class::Mcu {
                        let serial_device =
                            SerialDevice::new(current_task, serial_proxy.into_channel().into())
                                .expect("Can create SerialDevice wrapper");

                        // TODO This will register with an incorrect device number. We should be
                        // dynamically registering a major device and this should be minor device 1
                        // of that major device.
                        let registry = &current_task.kernel().device_registry;
                        registry
                            .register_dyn_device(
                                current_task.kernel().kthreads.unlocked_for_async().deref_mut(),
                                current_task,
                                "ttyHS1".into(),
                                registry.objects.tty_class(),
                                DeviceDirectory::new,
                                serial_device,
                            )
                            .expect("Can register serial device");
                        break;
                    }
                }
            }
            Ok(None) => {
                break;
            }
            Err(e) => {
                log_error!("Serial driver stream ended with error: {:}", e);
                break;
            }
        }
    }
}
