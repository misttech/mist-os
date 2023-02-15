// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A safe rust wrapper for creating and using ramdisks.

#![deny(missing_docs)]
use {
    anyhow::{anyhow, Context as _, Error},
    fidl::endpoints::{ClientEnd, Proxy as _},
    fidl_fuchsia_device::{ControllerMarker, ControllerProxy, ControllerSynchronousProxy},
    fidl_fuchsia_hardware_block as fhardware_block,
    fidl_fuchsia_hardware_ramdisk::{Guid, RamdiskControllerMarker},
    fidl_fuchsia_io as fio,
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
    fuchsia_zircon as zx,
};

const GUID_LEN: usize = 16;
const DEV_PATH: &str = "/dev";
const RAMCTL_PATH: &str = "sys/platform/00:00:2d/ramctl";
const BLOCK_EXTENSION: &str = "block";

/// A type to help construct a [`RamdeviceClient`] optionally from a VMO.
pub struct RamdiskClientBuilder {
    ramdisk_source: RamdiskSource,
    block_size: u64,
    dev_root: Option<fio::DirectoryProxy>,
    guid: Option<[u8; GUID_LEN]>,
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
            dev_root: None,
            guid: None,
        }
    }

    /// Create a new ramdisk builder with a vmo
    pub fn new_with_vmo(vmo: zx::Vmo, block_size: Option<u64>) -> Self {
        Self {
            ramdisk_source: RamdiskSource::Vmo { vmo },
            block_size: block_size.unwrap_or(0),
            dev_root: None,
            guid: None,
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

    /// Create the ramdisk.
    pub async fn build(self) -> Result<RamdiskClient, Error> {
        let Self { ramdisk_source, block_size, dev_root, guid } = self;
        let dev_root = if let Some(dev_root) = dev_root {
            dev_root
        } else {
            fuchsia_fs::directory::open_in_namespace(DEV_PATH, fio::OpenFlags::RIGHT_READABLE)
                .with_context(|| format!("open {}", DEV_PATH))?
        };
        let ramdisk_controller =
            device_watcher::recursive_wait_and_open::<RamdiskControllerMarker>(
                &dev_root,
                RAMCTL_PATH,
            )
            .await
            .with_context(|| format!("waiting for {}", RAMCTL_PATH))?;
        let mut type_guid = guid.map(|guid| Guid { value: guid });
        let name = match ramdisk_source {
            RamdiskSource::Vmo { vmo } => ramdisk_controller
                .create_from_vmo_with_params(vmo, block_size, type_guid.as_mut())
                .await?
                .map_err(zx::Status::from_raw)?,
            RamdiskSource::Size { block_count } => ramdisk_controller
                .create(block_size, block_count, type_guid.as_mut())
                .await?
                .map_err(zx::Status::from_raw)?,
        };
        let name = name.ok_or_else(|| anyhow!("Failed to get instance name"))?;
        RamdiskClient::new(dev_root, &name).await
    }
}

/// A client for managing a ramdisk. This can be created with the [`RamdiskClient::create`]
/// function or through the type returned by [`RamdiskClient::builder`] to specify additional
/// options.
pub struct RamdiskClient {
    block_dir: Option<fio::DirectoryProxy>,
    block_controller: Option<ControllerProxy>,
    ramdisk_controller: Option<ControllerProxy>,
    // TODO(nikitajindal): Remove this after soft transition in v/g.
    path: String,
}

impl RamdiskClient {
    async fn new(dev_root: fio::DirectoryProxy, instance_name: &str) -> Result<Self, Error> {
        let ramdisk_path = format!("{RAMCTL_PATH}/{instance_name}");
        let block_path = format!("{ramdisk_path}/{BLOCK_EXTENSION}");

        // Wait for ramdisk path to appear
        let ramdisk_controller =
            device_watcher::recursive_wait_and_open::<ControllerMarker>(&dev_root, &ramdisk_path)
                .await
                .with_context(|| format!("waiting for {}", &ramdisk_path))?;

        // Wait for the block path to appear
        let block_dir = device_watcher::recursive_wait_and_open_directory(&dev_root, &block_path)
            .await
            .with_context(|| format!("waiting for {}", &block_path))?;

        let block_controller =
            connect_to_named_protocol_at_dir_root::<ControllerMarker>(&block_dir, ".")?;

        let path = block_controller
            .get_topological_path()
            .await
            .context("transport error on get_topological_path")?
            .map_err(zx::Status::from_raw)
            .context("get_topological_path failed")?;

        Ok(RamdiskClient {
            block_dir: Some(block_dir),
            block_controller: Some(block_controller),
            ramdisk_controller: Some(ramdisk_controller),
            path,
        })
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
        self.block_controller.as_ref()
    }

    /// Take the block controller.
    pub fn take_controller(&mut self) -> Option<ControllerProxy> {
        self.block_controller.take()
    }

    /// Get the topological path of the block device backed by the ramdisk.
    pub fn get_path(&self) -> &str {
        &self.path
    }

    /// Get a reference to the block directory proxy.
    pub fn as_dir(&self) -> Option<&fio::DirectoryProxy> {
        self.block_dir.as_ref()
    }

    /// Take the block directory proxy.
    pub fn take_dir(&mut self) -> Option<fio::DirectoryProxy> {
        self.block_dir.take()
    }

    /// Get an open channel to the underlying ramdevice.
    pub async fn open(
        &self,
    ) -> Result<fidl::endpoints::ClientEnd<fhardware_block::BlockMarker>, Error> {
        // At this point, we have already waited on the block path to appear so
        // we can directly open a connection to the ramdevice.
        // TODO(https://fxbug.dev/112484): In order to allow multiplexing to be removed, use
        // connect_to_device_fidl to connect to the BlockProxy instead of connect_to_.._dir_root.
        // Requires downstream work.
        let block_dir = self.as_dir().ok_or_else(|| anyhow!("invalid directory proxy"))?;
        let block_proxy =
            connect_to_named_protocol_at_dir_root::<fhardware_block::BlockMarker>(block_dir, ".")?;
        let block_client_end = ClientEnd::<fhardware_block::BlockMarker>::new(
            block_proxy.into_channel().unwrap().into(),
        );
        Ok(block_client_end)
    }

    /// Remove the underlying ramdisk. This deallocates all resources for this ramdisk, which will
    /// remove all data written to the associated ramdisk.
    pub async fn destroy(mut self) -> Result<(), Error> {
        let proxy =
            self.ramdisk_controller.take().ok_or_else(|| anyhow!("ControllerProxy is invalid"))?;
        Ok(proxy.schedule_unbind().await?.map_err(zx::Status::from_raw)?)
    }

    /// Consume the RamdiskClient without destroying the underlying ramdisk. The caller must
    /// manually destroy the ramdisk device after calling this function.
    ///
    /// This should be used instead of `std::mem::forget`, as the latter will leak memory.
    pub fn forget(mut self) -> Result<(), Error> {
        let _proxy =
            self.ramdisk_controller.take().ok_or_else(|| anyhow!("ControllerProxy is invalid"))?;
        Ok(())
    }
}

impl Drop for RamdiskClient {
    fn drop(&mut self) {
        let proxy = self.ramdisk_controller.take();
        if let Some(proxy) = proxy {
            let sync_proxy = ControllerSynchronousProxy::new(proxy.into_channel().unwrap().into());
            let _ = sync_proxy.schedule_unbind(zx::Time::INFINITE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ramdisk_dir = ramdisk.as_dir().expect("invalid directory proxy");
        fuchsia_fs::directory::readdir(ramdisk_dir).await.expect("failed to readdir");
        ramdisk.destroy().await.expect("failed to destroy the ramdisk");
    }

    #[fuchsia::test]
    async fn create_with_dev_root_and_guid_get_dir_proxy_destroy() {
        let dev_root =
            fuchsia_fs::directory::open_in_namespace(DEV_PATH, fio::OpenFlags::RIGHT_READABLE)
                .with_context(|| format!("open {}", DEV_PATH))
                .expect("failed to create directory proxy");
        let ramdisk = RamdiskClient::builder(512, 2048)
            .dev_root(dev_root)
            .guid(TEST_GUID)
            .build()
            .await
            .expect("failed to create ramdisk");
        let ramdisk_dir = ramdisk.as_dir().expect("invalid directory proxy");
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
        let client = ramdisk.open().await.unwrap().into_proxy().unwrap();
        client.get_info().await.expect("get_info failed").unwrap();
        ramdisk.destroy().await.expect("failed to destroy the ramdisk");
        // The ramdisk will be scheduled to be unbound, so `client` may be valid for some time.
    }

    #[fuchsia::test]
    async fn create_open_forget() {
        let ramdisk = RamdiskClient::create(512, 2048).await.unwrap();
        let client = ramdisk.open().await.unwrap().into_proxy().unwrap();
        client.get_info().await.expect("get_info failed").unwrap();
        assert!(ramdisk.forget().is_ok());
        // We should succeed calling `get_info` as the ramdisk should still exist.
        client.get_info().await.expect("get_info failed").unwrap();
    }

    #[fuchsia::test]
    async fn create_describe_destroy() {
        let ramdisk = RamdiskClient::create(512, 2048).await.unwrap();
        let client_end = ramdisk.open().await.unwrap();

        // Ask it to describe itself using the Node interface.
        //
        // TODO(https://fxbug.dev/112484): this relies on multiplexing.
        let client_end =
            fidl::endpoints::ClientEnd::<fio::NodeMarker>::new(client_end.into_channel());
        let proxy = client_end.into_proxy().unwrap();
        let protocol = proxy.query().await.expect("failed to get node info");
        assert_eq!(protocol, fio::NODE_PROTOCOL_NAME.as_bytes());

        ramdisk.destroy().await.expect("failed to destroy the ramdisk");
    }
}
