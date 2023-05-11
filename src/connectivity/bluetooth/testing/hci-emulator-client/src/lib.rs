// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context as _, Error};
use fidl::endpoints::Proxy as _;
use fidl_fuchsia_bluetooth_test::{
    EmulatorError, EmulatorSettings, HciEmulatorMarker, HciEmulatorProxy,
};
use fidl_fuchsia_device::{ControllerMarker, ControllerProxy};
use fidl_fuchsia_hardware_bluetooth::{
    EmulatorMarker, EmulatorProxy, HostMarker, HostProxy, VirtualControllerMarker,
};
use fidl_fuchsia_io::DirectoryProxy;
use fuchsia_async::{DurationExt as _, TimeoutExt as _};
use fuchsia_bluetooth::constants::{
    DEV_DIR, HOST_DEVICE_DIR, INTEGRATION_TIMEOUT as WATCH_TIMEOUT,
};
use fuchsia_zircon as zx;
use futures::TryFutureExt as _;
use tracing::error;

pub mod types;

const EMULATOR_DEVICE_DIR: &str = "class/bt-emulator";

/// Represents a bt-hci device emulator. Instances of this type can be used manage the
/// bt-hci-emulator driver within the test device hierarchy. The associated driver instance gets
/// unbound and all bt-hci and bt-emulator device instances destroyed when
/// `destroy_and_wait()` resolves successfully.
/// `destroy_and_wait()` MUST be called for proper clean up of the emulator device.
pub struct Emulator {
    /// This will have a value when the emulator is instantiated and will be reset to None
    /// in `destroy_and_wait()`. This is so the destructor can assert that the TestDevice has been
    /// destroyed.
    dev: Option<TestDevice>,
    hci_emulator: HciEmulatorProxy,
}

impl Emulator {
    /// Returns the default settings.
    // TODO(armansito): Consider defining a library type for EmulatorSettings.
    pub fn default_settings() -> EmulatorSettings {
        EmulatorSettings {
            address: None,
            hci_config: None,
            extended_advertising: None,
            acl_buffer_settings: None,
            le_acl_buffer_settings: None,
            ..Default::default()
        }
    }

    /// Publish a new bt-emulator device and return a handle to it. No corresponding bt-hci device
    /// will be published; to do so it must be explicitly configured and created with a call to
    /// `publish()`. If `realm` is present, the device will be created inside it, otherwise it will
    /// be created using the `/dev` directory in the component's namespace.
    pub async fn create(dev_directory: DirectoryProxy) -> Result<Emulator, Error> {
        let (dev, hci_emulator) = TestDevice::create(dev_directory)
            .await
            .context(format!("Error creating test device"))?;
        Ok(Emulator { dev: Some(dev), hci_emulator })
    }

    /// Publish a bt-emulator and a bt-hci device using the default emulator settings. If `realm`
    /// is present, the device will be created inside it, otherwise it will be created using the
    /// `/dev` directory in the component's namespace.
    pub async fn create_and_publish(dev_directory: DirectoryProxy) -> Result<Emulator, Error> {
        let fake_dev = Self::create(dev_directory).await?;
        fake_dev.publish(Self::default_settings()).await?;
        Ok(fake_dev)
    }

    /// Sends a publish message to the emulator. This is a convenience method that internally
    /// handles the FIDL binding error.
    pub async fn publish(&self, settings: EmulatorSettings) -> Result<(), Error> {
        self.emulator()
            .publish(&settings)
            .await
            .context("publish transport")?
            .map_err(|e: EmulatorError| format_err!("failed to publish bt-hci device: {:#?}", e))
    }

    /// Sends a publish message emulator and returns a Future that resolves when a bt-host device is
    /// published. Note that this requires the bt-host driver to be installed. On success, returns a
    /// proxy to the bt-host device.
    pub async fn publish_and_wait_for_host(
        &self,
        settings: EmulatorSettings,
    ) -> Result<HostProxy, Error> {
        let () = self.publish(settings).await?;
        let dev = self.dev.as_ref().expect("emulator device accessed after it was destroyed!");
        let topo = dev.get_topological_path().await?;
        let TestDevice { dev_directory, controller: _, emulator: _ } = dev;
        let host_dir = fuchsia_fs::directory::open_directory_no_describe(
            dev_directory,
            HOST_DEVICE_DIR,
            fuchsia_fs::OpenFlags::empty(),
        )?;
        let host = device_watcher::wait_for_device_with(
            &host_dir,
            |device_watcher::DeviceInfo { filename, topological_path }| {
                topological_path.starts_with(&topo).then(|| {
                    fuchsia_component::client::connect_to_named_protocol_at_dir_root::<HostMarker>(
                        &host_dir, filename,
                    )
                })
            },
        )
        .on_timeout(WATCH_TIMEOUT, || Err(format_err!("timed out waiting for device to appear")))
        .await??;
        Ok(host)
    }

    /// Sends the test device a destroy message which will unbind the driver.
    /// This will wait for the test device to be unpublished from devfs.
    pub async fn destroy_and_wait(&mut self) -> Result<(), Error> {
        self.dev
            .take()
            .expect("attempted to destroy an already destroyed emulator device")
            .destroy_and_wait()
            .await
    }

    pub async fn get_topological_path(&self) -> Result<String, Error> {
        let dev = self.dev.as_ref().expect("emulator device accessed after it was destroyed!");
        dev.get_topological_path().await
    }

    /// Returns a reference to the fuchsia.bluetooth.test.HciEmulator protocol proxy.
    pub fn emulator(&self) -> &HciEmulatorProxy {
        &self.hci_emulator
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        if self.dev.is_some() {
            error!("Did not call destroy() on Emulator");
        }
    }
}

// Represents the test device. `destroy()` MUST be called explicitly to remove the device.
// The device will be removed asynchronously so the caller cannot rely on synchronous
// execution of destroy() to know about device removal. Instead, the caller should watch for the
// device path to be removed.
struct TestDevice {
    dev_directory: DirectoryProxy,
    controller: ControllerProxy,
    emulator: EmulatorProxy,
}

impl TestDevice {
    // Creates a new device as a child of the emulator controller device and obtain the HciEmulator
    // protocol channel.
    async fn create(
        dev_directory: DirectoryProxy,
    ) -> Result<(TestDevice, HciEmulatorProxy), Error> {
        // 0x30 => fuchsia.platform.BIND_PLATFORM_DEV_DID.BT_HCI_EMULATOR
        const CONTROL_DEVICE: &str = "sys/platform/00:00:30/bt_hci_virtual";

        let controller = device_watcher::recursive_wait_and_open::<VirtualControllerMarker>(
            &dev_directory,
            CONTROL_DEVICE,
        )
        .await
        .with_context(|| format!("failed to open {}", CONTROL_DEVICE))?;
        let name = controller
            .create_emulator()
            .map_err(Error::from)
            .on_timeout(WATCH_TIMEOUT.after_now(), || {
                Err(format_err!("timed out waiting for emulator to create test device"))
            })
            .await?
            .map_err(zx::Status::from_raw)?
            .ok_or_else(|| {
                format_err!("name absent from EmulatorController::Create FIDL response")
            })?;

        let emulator_dir = fuchsia_fs::directory::open_directory_no_describe(
            &dev_directory,
            EMULATOR_DEVICE_DIR,
            fuchsia_fs::OpenFlags::empty(),
        )?;

        // Wait until a bt-emulator device gets published under our test device.
        let directory = device_watcher::wait_for_device_with(
            &emulator_dir,
            |device_watcher::DeviceInfo { filename, topological_path }| {
                let topological_path = topological_path.strip_prefix(DEV_DIR)?;
                let topological_path = topological_path.strip_prefix('/')?;
                let topological_path = topological_path.strip_prefix(CONTROL_DEVICE)?;
                let topological_path = topological_path.strip_prefix('/')?;
                let topological_path = topological_path.strip_prefix(&name)?;
                let _: &str = topological_path;
                Some(fuchsia_fs::directory::open_directory_no_describe(
                    &emulator_dir,
                    filename,
                    fuchsia_fs::OpenFlags::empty(),
                ))
            },
        )
        .on_timeout(WATCH_TIMEOUT, || Err(format_err!("timed out waiting for device to appear")))
        .await??;

        let controller = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
            ControllerMarker,
        >(&directory, fidl_fuchsia_device_fs::DEVICE_CONTROLLER_NAME)?;
        let emulator = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
            EmulatorMarker,
        >(&directory, fidl_fuchsia_device_fs::DEVICE_PROTOCOL_NAME)?;

        // Open a HciEmulator protocol channel.
        let (proxy, server_end) = fidl::endpoints::create_proxy::<HciEmulatorMarker>()?;
        let () = emulator.open(server_end)?;
        Ok((Self { dev_directory, controller, emulator }, proxy))
    }

    /// Sends the test device a destroy message which will unbind the driver.
    /// This will wait for the test device to be unpublished from devfs.
    pub async fn destroy_and_wait(&mut self) -> Result<(), Error> {
        let () = self.controller.schedule_unbind().await?.map_err(zx::Status::from_raw)?;
        let _: (zx::Signals, zx::Signals) = futures::future::try_join(
            self.controller.as_channel().on_closed(),
            self.emulator.as_channel().on_closed(),
        )
        .await?;
        Ok(())
    }

    pub async fn get_topological_path(&self) -> Result<String, Error> {
        self.controller
            .get_topological_path()
            .await
            .context("get topological path transport")?
            .map_err(zx::Status::from_raw)
            .context("get topological path")
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl_fuchsia_driver_test as fdt, fuchsia,
        fuchsia_component_test::RealmBuilder,
        fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    };

    const HCI_DEVICE_DIR: &str = "class/bt-hci";

    fn default_settings() -> EmulatorSettings {
        EmulatorSettings {
            address: None,
            hci_config: None,
            extended_advertising: None,
            acl_buffer_settings: None,
            le_acl_buffer_settings: None,
            ..Default::default()
        }
    }

    #[fuchsia::test]
    async fn test_publish_lifecycle() {
        // We use these watchers to verify the addition and removal of these devices as tied to the
        // lifetime of the Emulator instance we create below.
        let emul_dev: EmulatorProxy;
        let hci_dev: HciEmulatorProxy;

        let realm = RealmBuilder::new().await.expect("realm builder");
        let _: &RealmBuilder =
            realm.driver_test_realm_setup().await.expect("driver test realm setup");
        let realm = realm.build().await.expect("failed to build realm");
        let args = fdt::RealmArgs {
            root_driver: Some("fuchsia-boot:///#meta/platform-bus.cm".to_string()),
            ..Default::default()
        };
        realm.driver_test_realm_start(args).await.expect("driver test realm start");

        {
            let dev_dir = realm.driver_test_realm_connect_to_dev().unwrap();
            let mut fake_dev =
                Emulator::create(dev_dir).await.expect("Failed to construct Emulator");
            let Emulator { dev, hci_emulator: _ } = &fake_dev;
            let dev = dev.as_ref().expect("emulator device exists");
            let topo = dev
                .get_topological_path()
                .await
                .expect("Failed to obtain topological path for Emulator");
            let TestDevice { dev_directory, controller: _, emulator: _ } = dev;

            // A bt-emulator device should already exist by now.
            {
                let emulator_dir = fuchsia_fs::directory::open_directory_no_describe(
                    dev_directory,
                    EMULATOR_DEVICE_DIR,
                    fuchsia_fs::OpenFlags::empty(),
                )
                .expect("open emulator directory");
                emul_dev = device_watcher::wait_for_device_with(
                    &emulator_dir,
                    |device_watcher::DeviceInfo { filename, topological_path }| {
                        topological_path.starts_with(&topo).then(|| {
                            fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
                                EmulatorMarker,
                            >(&emulator_dir, filename)
                            .expect("failed to connect to device")
                        })
                    },
                )
                .on_timeout(WATCH_TIMEOUT, || panic!("timed out waiting for device to appear"))
                .await
                .expect("failed to watch for device");
            }

            // Send a publish message to the device. This call should succeed and result in a new
            // bt-hci device.
            let () = fake_dev
                .publish(default_settings())
                .await
                .expect("Failed to send Publish message to emulator device");
            {
                let hci_dir = fuchsia_fs::directory::open_directory_no_describe(
                    dev_directory,
                    HCI_DEVICE_DIR,
                    fuchsia_fs::OpenFlags::empty(),
                )
                .expect("open hci directory");
                hci_dev = device_watcher::wait_for_device_with(
                    &hci_dir,
                    |device_watcher::DeviceInfo { filename, topological_path }| {
                        topological_path.starts_with(&topo).then(|| {
                            fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
                                HciEmulatorMarker,
                            >(&hci_dir, filename)
                            .expect("failed to connect to device")
                        })
                    },
                )
                .on_timeout(WATCH_TIMEOUT, || panic!("timed out waiting for device to appear"))
                .await
                .expect("failed to watch for device");
            }

            // Once a device is published, it should not be possible to publish again while the
            // HciEmulator channel is open.
            let result = fake_dev
                .emulator()
                .publish(&default_settings())
                .await
                .expect("Failed to send second Publish message to emulator device");
            assert_eq!(Err(EmulatorError::HciAlreadyPublished), result);

            fake_dev.destroy_and_wait().await.expect("Expected test device to be removed");
        }

        // Both devices should be destroyed when `fake_dev` gets dropped.
        let _: (zx::Signals, zx::Signals) = futures::future::try_join(
            emul_dev.as_channel().on_closed(),
            hci_dev.as_channel().on_closed(),
        )
        .on_timeout(WATCH_TIMEOUT, || panic!("timed out waiting for device to close"))
        .await
        .expect("on closed");
    }
}
