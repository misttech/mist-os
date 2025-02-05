// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A safe rust wrapper for creating and using ramdisks.

#![deny(missing_docs)]
use anyhow::{anyhow, Context as _, Error};
use fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker as _, Proxy as _};
use fidl_fuchsia_device::{ControllerMarker, ControllerProxy, ControllerSynchronousProxy};
use fidl_fuchsia_hardware_ramdisk::{Guid, RamdiskControllerMarker};
use fs_management::filesystem::{BlockConnector, DirBasedBlockConnector};
use fuchsia_component::client::{
    connect_to_named_protocol_at_dir_root, connect_to_protocol_at_dir_svc, Service,
};
use {
    fidl_fuchsia_hardware_block as fhardware_block, fidl_fuchsia_hardware_block_volume as fvolume,
    fidl_fuchsia_hardware_ramdisk as framdisk, fidl_fuchsia_io as fio,
};

const GUID_LEN: usize = 16;
const DEV_PATH: &str = "/dev";
const RAMCTL_PATH: &str = "sys/platform/ram-disk/ramctl";
const BLOCK_EXTENSION: &str = "block";

/// A type to help construct a [`RamdeviceClient`] optionally from a VMO.
pub struct RamdiskClientBuilder {
    ramdisk_source: RamdiskSource,
    block_size: u64,
    dev_root: Option<fio::DirectoryProxy>,
    guid: Option<[u8; GUID_LEN]>,
    use_v2: bool,
    ramdisk_service: Option<fio::DirectoryProxy>,

    // Whether to publish this ramdisk as a fuchsia.hardware.block.volume.Service service.  This
    // only works for the v2 driver.
    publish: bool,
}

enum RamdiskSource {
    Vmo { vmo: zx::Vmo },
    Size { block_count: u64 },
}

impl RamdiskClientBuilder {
    /// Create a new ramdisk builder
    pub fn new(block_size: u64, block_count: u64) -> Self {
        Self {
            ramdisk_source: RamdiskSource::Size { block_count },
            block_size,
            guid: None,
            dev_root: None,
            use_v2: false,
            ramdisk_service: None,
            publish: false,
        }
    }

    /// Create a new ramdisk builder with a vmo
    pub fn new_with_vmo(vmo: zx::Vmo, block_size: Option<u64>) -> Self {
        Self {
            ramdisk_source: RamdiskSource::Vmo { vmo },
            block_size: block_size.unwrap_or(0),
            guid: None,
            dev_root: None,
            use_v2: false,
            ramdisk_service: None,
            publish: false,
        }
    }

    /// Use the given directory as "/dev" instead of opening "/dev" from the environment.
    pub fn dev_root(mut self, dev_root: fio::DirectoryProxy) -> Self {
        self.dev_root = Some(dev_root);
        self
    }

    /// Initialize the ramdisk with the given GUID, which can be queried from the ramdisk instance.
    pub fn guid(mut self, guid: [u8; GUID_LEN]) -> Self {
        self.guid = Some(guid);
        self
    }

    /// Use the V2 ramdisk driver.
    pub fn use_v2(mut self) -> Self {
        self.use_v2 = true;
        self
    }

    /// Specifies the ramdisk service.
    pub fn ramdisk_service(mut self, service: fio::DirectoryProxy) -> Self {
        self.ramdisk_service = Some(service);
        self
    }

    /// Publish this ramdisk as a fuchsia.hardware.block.volume.Service service.
    pub fn publish(mut self) -> Self {
        self.publish = true;
        self
    }

    /// Create the ramdisk.
    pub async fn build(self) -> Result<RamdiskClient, Error> {
        let Self { ramdisk_source, block_size, guid, dev_root, use_v2, ramdisk_service, publish } =
            self;

        if use_v2 {
            // Pick the first service instance we find.
            let service = match ramdisk_service {
                Some(s) => {
                    Service::from_service_dir_proxy(s, fidl_fuchsia_hardware_ramdisk::ServiceMarker)
                }
                None => Service::open(fidl_fuchsia_hardware_ramdisk::ServiceMarker)?,
            };
            let ramdisk_controller = service.watch_for_any().await?.connect_to_controller()?;

            let type_guid = guid.map(|guid| Guid { value: guid });

            let options = match ramdisk_source {
                RamdiskSource::Vmo { vmo } => framdisk::Options {
                    vmo: Some(vmo),
                    block_size: if block_size == 0 {
                        None
                    } else {
                        Some(block_size.try_into().unwrap())
                    },
                    type_guid,
                    publish: Some(publish),
                    ..Default::default()
                },
                RamdiskSource::Size { block_count } => framdisk::Options {
                    block_count: Some(block_count),
                    block_size: Some(block_size.try_into().unwrap()),
                    type_guid,
                    publish: Some(publish),
                    ..Default::default()
                },
            };

            let (outgoing, event) =
                ramdisk_controller.create(options).await?.map_err(|s| zx::Status::from_raw(s))?;

            RamdiskClient::new_v2(outgoing.into_proxy(), event)
        } else {
            let dev_root = if let Some(dev_root) = dev_root {
                dev_root
            } else {
                fuchsia_fs::directory::open_in_namespace(DEV_PATH, fio::PERM_READABLE)
                    .with_context(|| format!("open {}", DEV_PATH))?
            };
            let ramdisk_controller = device_watcher::recursive_wait_and_open::<
                RamdiskControllerMarker,
            >(&dev_root, RAMCTL_PATH)
            .await
            .with_context(|| format!("waiting for {}", RAMCTL_PATH))?;
            let type_guid = guid.map(|guid| Guid { value: guid });
            let name = match ramdisk_source {
                RamdiskSource::Vmo { vmo } => ramdisk_controller
                    .create_from_vmo_with_params(vmo, block_size, type_guid.as_ref())
                    .await?
                    .map_err(zx::Status::from_raw)
                    .context("creating ramdisk from vmo")?,
                RamdiskSource::Size { block_count } => ramdisk_controller
                    .create(block_size, block_count, type_guid.as_ref())
                    .await?
                    .map_err(zx::Status::from_raw)
                    .with_context(|| format!("creating ramdisk with {} blocks", block_count))?,
            };
            let name = name.ok_or_else(|| anyhow!("Failed to get instance name"))?;
            RamdiskClient::new(dev_root, &name).await
        }
    }
}

/// A client for managing a ramdisk. This can be created with the [`RamdiskClient::create`]
/// function or through the type returned by [`RamdiskClient::builder`] to specify additional
/// options.
pub enum RamdiskClient {
    /// V1
    V1 {
        /// The directory backing the block driver.
        block_dir: Option<fio::DirectoryProxy>,

        /// The device controller for the block device.
        block_controller: Option<ControllerProxy>,

        /// The device controller for the ramdisk.
        ramdisk_controller: Option<ControllerProxy>,
    },
    /// V2
    V2 {
        /// The outgoing directory for the ram-disk.
        outgoing: fio::DirectoryProxy,

        /// The event that keeps the ramdisk alive.
        _event: zx::EventPair,
    },
}

impl RamdiskClient {
    async fn new(dev_root: fio::DirectoryProxy, instance_name: &str) -> Result<Self, Error> {
        let ramdisk_path = format!("{RAMCTL_PATH}/{instance_name}");
        let ramdisk_controller_path = format!("{ramdisk_path}/device_controller");
        let block_path = format!("{ramdisk_path}/{BLOCK_EXTENSION}");

        // Wait for ramdisk path to appear
        let ramdisk_controller = device_watcher::recursive_wait_and_open::<ControllerMarker>(
            &dev_root,
            &ramdisk_controller_path,
        )
        .await
        .with_context(|| format!("waiting for {}", &ramdisk_controller_path))?;

        // Wait for the block path to appear
        let block_dir = device_watcher::recursive_wait_and_open_directory(&dev_root, &block_path)
            .await
            .with_context(|| format!("waiting for {}", &block_path))?;

        let block_controller = connect_to_named_protocol_at_dir_root::<ControllerMarker>(
            &block_dir,
            "device_controller",
        )
        .with_context(|| {
            format!("opening block controller at {}/device_controller", &block_path)
        })?;

        Ok(Self::V1 {
            block_dir: Some(block_dir),
            block_controller: Some(block_controller),
            ramdisk_controller: Some(ramdisk_controller),
        })
    }

    fn new_v2(outgoing: fio::DirectoryProxy, event: zx::EventPair) -> Result<Self, Error> {
        Ok(Self::V2 { outgoing, _event: event })
    }

    /// Create a new ramdisk builder with the given block_size and block_count.
    pub fn builder(block_size: u64, block_count: u64) -> RamdiskClientBuilder {
        RamdiskClientBuilder::new(block_size, block_count)
    }

    /// Create a new ramdisk.
    pub async fn create(block_size: u64, block_count: u64) -> Result<Self, Error> {
        Self::builder(block_size, block_count).build().await
    }

    /// Get a reference to the block controller.
    pub fn as_controller(&self) -> Option<&ControllerProxy> {
        match self {
            Self::V1 { block_controller, .. } => block_controller.as_ref(),
            Self::V2 { .. } => None,
        }
    }

    /// Take the block controller.
    pub fn take_controller(&mut self) -> Option<ControllerProxy> {
        match self {
            Self::V1 { block_controller, .. } => block_controller.take(),
            Self::V2 { .. } => None,
        }
    }

    /// Get a reference to the block directory proxy.
    pub fn as_dir(&self) -> Option<&fio::DirectoryProxy> {
        match self {
            Self::V1 { block_dir, .. } => block_dir.as_ref(),
            Self::V2 { .. } => None,
        }
    }

    /// Take the block directory proxy.
    pub fn take_dir(&mut self) -> Option<fio::DirectoryProxy> {
        match self {
            Self::V1 { block_dir, .. } => block_dir.take(),
            Self::V2 { .. } => None,
        }
    }

    /// Get an open channel to the underlying ramdevice.
    pub fn open(&self) -> Result<fidl::endpoints::ClientEnd<fhardware_block::BlockMarker>, Error> {
        match self {
            Self::V1 { .. } => {
                // At this point, we have already waited on the block path to appear so we can
                // directly open a connection to the ramdevice.

                // TODO(https://fxbug.dev/42063787): In order to allow multiplexing to be removed,
                // use connect_to_device_fidl to connect to the BlockProxy instead of
                // connect_to_.._dir_root.  Requires downstream work.
                let block_dir = self.as_dir().ok_or_else(|| anyhow!("directory is invalid"))?;
                let block_proxy = connect_to_named_protocol_at_dir_root::<
                    fhardware_block::BlockMarker,
                >(block_dir, ".")?;
                let block_client_end = ClientEnd::<fhardware_block::BlockMarker>::new(
                    block_proxy.into_channel().unwrap().into(),
                );
                Ok(block_client_end)
            }
            Self::V2 { outgoing, .. } => {
                let block_proxy =
                    connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(outgoing)?;
                let block_client_end = ClientEnd::<fhardware_block::BlockMarker>::new(
                    block_proxy.into_channel().unwrap().into(),
                );
                Ok(block_client_end)
            }
        }
    }

    /// Gets a connector for the Block protocol of the ramdisk.
    pub fn connector(&self) -> Result<Box<dyn BlockConnector>, Error> {
        match self {
            Self::V1 { .. } => {
                // At this point, we have already waited on the block path to appear so we can
                // directly open a connection to the ramdevice.

                // TODO(https://fxbug.dev/42063787): In order to allow multiplexing to be removed,
                // use connect_to_device_fidl to connect to the BlockProxy instead of
                // connect_to_.._dir_root.  Requires downstream work.
                let block_dir = fuchsia_fs::directory::clone(
                    self.as_dir().ok_or_else(|| anyhow!("directory is invalid"))?,
                )?;
                Ok(Box::new(DirBasedBlockConnector::new(block_dir, ".".to_string())))
            }
            Self::V2 { outgoing, .. } => {
                let block_dir = fuchsia_fs::directory::clone(outgoing)?;
                Ok(Box::new(DirBasedBlockConnector::new(
                    block_dir,
                    format!("svc/{}", fvolume::VolumeMarker::PROTOCOL_NAME),
                )))
            }
        }
    }

    /// Get an open channel to the underlying ramdevice.
    pub fn connect(
        &self,
        server_end: fidl::endpoints::ServerEnd<fhardware_block::BlockMarker>,
    ) -> Result<(), Error> {
        match self {
            Self::V1 { .. } => {
                let block_dir = self.as_dir().ok_or_else(|| anyhow!("directory is invalid"))?;
                Ok(block_dir.open3(
                    ".",
                    fio::Flags::empty(),
                    &fio::Options::default(),
                    server_end.into_channel(),
                )?)
            }
            Self::V2 { outgoing, .. } => Ok(outgoing.open3(
                &format!("svc/{}", fvolume::VolumeMarker::PROTOCOL_NAME),
                fio::Flags::empty(),
                &fio::Options::default(),
                server_end.into_channel(),
            )?),
        }
    }

    /// Get an open channel to the underlying ramdevice's controller.
    pub fn open_controller(
        &self,
    ) -> Result<fidl::endpoints::ClientEnd<fidl_fuchsia_device::ControllerMarker>, Error> {
        match self {
            Self::V1 { .. } => {
                let block_dir = self.as_dir().ok_or_else(|| anyhow!("directory is invalid"))?;
                let controller_proxy = connect_to_named_protocol_at_dir_root::<
                    fidl_fuchsia_device::ControllerMarker,
                >(block_dir, "device_controller")?;
                Ok(ClientEnd::new(controller_proxy.into_channel().unwrap().into()))
            }
            Self::V2 { .. } => Err(anyhow!("Not supported")),
        }
    }

    /// Starts unbinding the underlying ramdisk and returns before the device is removed. This
    /// deallocates all resources for this ramdisk, which will remove all data written to the
    /// associated ramdisk.
    pub async fn destroy(mut self) -> Result<(), Error> {
        match &mut self {
            Self::V1 { ramdisk_controller, .. } => {
                let ramdisk_controller = ramdisk_controller
                    .take()
                    .ok_or_else(|| anyhow!("ramdisk controller is invalid"))?;
                let () = ramdisk_controller
                    .schedule_unbind()
                    .await
                    .context("unbind transport")?
                    .map_err(zx::Status::from_raw)
                    .context("unbind response")?;
            }
            Self::V2 { .. } => {} // Dropping the event will destroy the device.
        }
        Ok(())
    }

    /// Unbinds the underlying ramdisk and waits for the device and all child devices to be removed.
    /// This deallocates all resources for this ramdisk, which will remove all data written to the
    /// associated ramdisk.
    pub async fn destroy_and_wait_for_removal(mut self) -> Result<(), Error> {
        match &mut self {
            Self::V1 { block_controller, ramdisk_controller, .. } => {
                // Calling `schedule_unbind` on the ramdisk controller initiates the unbind process
                // but doesn't wait for anything to complete. The unbinding process starts at the
                // ramdisk and propagates down through the child devices. FIDL connections are
                // closed during the unbind process so the ramdisk controller connection will be
                // closed before connections to the child block device. After unbinding, the drivers
                // are removed starting at the children and ending at the ramdisk.

                let block_controller = block_controller
                    .take()
                    .ok_or_else(|| anyhow!("block controller is invalid"))?;
                let ramdisk_controller = ramdisk_controller
                    .take()
                    .ok_or_else(|| anyhow!("ramdisk controller is invalid"))?;
                let () = ramdisk_controller
                    .schedule_unbind()
                    .await
                    .context("unbind transport")?
                    .map_err(zx::Status::from_raw)
                    .context("unbind response")?;
                let _: (zx::Signals, zx::Signals) = futures::future::try_join(
                    block_controller.on_closed(),
                    ramdisk_controller.on_closed(),
                )
                .await
                .context("on closed")?;
            }
            Self::V2 { .. } => {}
        }
        Ok(())
    }

    /// Consume the RamdiskClient without destroying the underlying ramdisk. The caller must
    /// manually destroy the ramdisk device after calling this function.
    ///
    /// This should be used instead of `std::mem::forget`, as the latter will leak memory.
    pub fn forget(mut self) -> Result<(), Error> {
        match &mut self {
            Self::V1 { ramdisk_controller, .. } => {
                let _: ControllerProxy = ramdisk_controller
                    .take()
                    .ok_or_else(|| anyhow!("ramdisk controller is invalid"))?;
                Ok(())
            }
            Self::V2 { .. } => Err(anyhow!("Not supported")),
        }
    }
}

impl BlockConnector for RamdiskClient {
    fn connect_volume(
        &self,
    ) -> Result<ClientEnd<fidl_fuchsia_hardware_block_volume::VolumeMarker>, Error> {
        self.open().map(|block| block.into_channel().into())
    }
}

impl Drop for RamdiskClient {
    fn drop(&mut self) {
        if let Self::V1 { ramdisk_controller, .. } = self {
            if let Some(ramdisk_controller) = ramdisk_controller.take() {
                let _: Result<Result<(), _>, _> = ControllerSynchronousProxy::new(
                    ramdisk_controller.into_channel().unwrap().into(),
                )
                .schedule_unbind(zx::MonotonicInstant::INFINITE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    // Note that if these tests flake, all downstream tests that depend on this crate may too.

    const TEST_GUID: [u8; GUID_LEN] = [
        0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09,
        0x10,
    ];

    #[fuchsia::test]
    async fn create_get_dir_proxy_destroy() {
        // just make sure all the functions are hooked up properly.
        let ramdisk =
            RamdiskClient::builder(512, 2048).build().await.expect("failed to create ramdisk");
        let ramdisk_dir = ramdisk.as_dir().expect("directory is invalid");
        fuchsia_fs::directory::readdir(ramdisk_dir).await.expect("failed to readdir");
        ramdisk.destroy().await.expect("failed to destroy the ramdisk");
    }

    #[fuchsia::test]
    async fn create_with_dev_root_and_guid_get_dir_proxy_destroy() {
        let dev_root = fuchsia_fs::directory::open_in_namespace(DEV_PATH, fio::PERM_READABLE)
            .with_context(|| format!("open {}", DEV_PATH))
            .expect("failed to create directory proxy");
        let ramdisk = RamdiskClient::builder(512, 2048)
            .dev_root(dev_root)
            .guid(TEST_GUID)
            .build()
            .await
            .expect("failed to create ramdisk");
        let ramdisk_dir = ramdisk.as_dir().expect("directory is invalid");
        fuchsia_fs::directory::readdir(ramdisk_dir).await.expect("failed to readdir");
        ramdisk.destroy().await.expect("failed to destroy the ramdisk");
    }

    #[fuchsia::test]
    async fn create_with_guid_get_dir_proxy_destroy() {
        let ramdisk = RamdiskClient::builder(512, 2048)
            .guid(TEST_GUID)
            .build()
            .await
            .expect("failed to create ramdisk");
        let ramdisk_dir = ramdisk.as_dir().expect("invalid directory proxy");
        fuchsia_fs::directory::readdir(ramdisk_dir).await.expect("failed to readdir");
        ramdisk.destroy().await.expect("failed to destroy the ramdisk");
    }

    #[fuchsia::test]
    async fn create_open_destroy() {
        let ramdisk = RamdiskClient::create(512, 2048).await.unwrap();
        let client = ramdisk.open().unwrap().into_proxy();
        client.get_info().await.expect("get_info failed").unwrap();
        ramdisk.destroy().await.expect("failed to destroy the ramdisk");
        // The ramdisk will be scheduled to be unbound, so `client` may be valid for some time.
    }

    #[fuchsia::test]
    async fn create_open_forget() {
        let ramdisk = RamdiskClient::create(512, 2048).await.unwrap();
        let client = ramdisk.open().unwrap().into_proxy();
        client.get_info().await.expect("get_info failed").unwrap();
        assert!(ramdisk.forget().is_ok());
        // We should succeed calling `get_info` as the ramdisk should still exist.
        client.get_info().await.expect("get_info failed").unwrap();
    }

    #[fuchsia::test]
    async fn destroy_and_wait_for_removal() {
        let mut ramdisk = RamdiskClient::create(512, 2048).await.unwrap();
        let dir = ramdisk.take_dir().unwrap();

        assert_matches!(
            fuchsia_fs::directory::readdir(&dir).await.unwrap().as_slice(),
            [
                fuchsia_fs::directory::DirEntry {
                    name: name1,
                    kind: fuchsia_fs::directory::DirentKind::File,
                },
                fuchsia_fs::directory::DirEntry {
                    name: name2,
                    kind: fuchsia_fs::directory::DirentKind::File,
                },
            ] if [name1, name2] == [
                fidl_fuchsia_device_fs::DEVICE_CONTROLLER_NAME,
                fidl_fuchsia_device_fs::DEVICE_PROTOCOL_NAME,
              ]
        );

        let () = ramdisk.destroy_and_wait_for_removal().await.unwrap();

        assert_matches!(fuchsia_fs::directory::readdir(&dir).await.unwrap().as_slice(), []);
    }
}
