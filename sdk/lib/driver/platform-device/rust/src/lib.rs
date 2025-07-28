// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![deny(missing_docs)]
//! PlatformDevice interface.

use fidl::{Persistable, Serializable};
use fidl_fuchsia_hardware_platform_device as pdev_fidl;
use log::error;
use mmio::region::MmioRegion;
use mmio::vmo::{VmoMapping, VmoMemory};
use mmio::MmioSplit;
use std::future::Future;
use std::sync::Arc;
use zx_status::Status;

/// PlatformDevice interface.
pub trait PlatformDevice {
    /// The type of the [Mmio] implementation returned by this platform device.
    type Mmio: MmioSplit + Send + Sync;

    /// Maps an MMIO region by its id.
    fn map_mmio_by_id(&self, id: u32) -> impl Future<Output = Result<Self::Mmio, Status>>;

    /// Maps MMIO memory by its name.
    fn map_mmio_by_name(&self, name: &str) -> impl Future<Output = Result<Self::Mmio, Status>>;

    /// Gets typed metadata associated with this platform device.
    fn get_typed_metadata<T: Persistable + Serializable>(
        &self,
    ) -> impl Future<Output = Result<T, Status>>;
}

impl PlatformDevice for pdev_fidl::DeviceProxy {
    type Mmio = MmioRegion<VmoMemory, Arc<VmoMemory>>;

    async fn map_mmio_by_id(&self, id: u32) -> Result<Self::Mmio, Status> {
        let mmio = self
            .get_mmio_by_id(id)
            .await
            .map_err(|err| {
                error!("Could not get mmio for id {id}: {err}");
                Status::INTERNAL
            })?
            .map_err(Status::from_raw)?;
        map_mmio(mmio)
    }

    async fn map_mmio_by_name(&self, name: &str) -> Result<Self::Mmio, Status> {
        let mmio = self
            .get_mmio_by_name(name)
            .await
            .map_err(|err| {
                error!("Could not get mmio for name {name}: {err}");
                Status::INTERNAL
            })?
            .map_err(Status::from_raw)?;
        map_mmio(mmio)
    }

    async fn get_typed_metadata<T: Persistable + Serializable>(&self) -> Result<T, Status> {
        let name = T::SERIALIZABLE_NAME;
        let metadata = self
            .get_metadata(name)
            .await
            .map_err(|err| {
                error!("Failed to get metadata from pdev: {name} {err}");
                Status::INTERNAL
            })?
            .map_err(Status::from_raw)?;
        fidl::unpersist(&metadata).map_err(|err| {
            error!("Failed to parse pdev metadata: {err}");
            Status::INVALID_ARGS
        })
    }
}

fn map_mmio(mmio: pdev_fidl::Mmio) -> Result<MmioRegion<VmoMemory, Arc<VmoMemory>>, Status> {
    let (Some(vmo), Some(offset), Some(size)) = (mmio.vmo, mmio.offset, mmio.size) else {
        error!("Mmio device missing vmo, offset or size");
        return Err(Status::INTERNAL);
    };
    let offset = offset as usize;
    let size = size as usize;

    let mmio = VmoMapping::map(offset, size, vmo).map_err(|err| {
        error!("Failed to map Mmio memory for vmo: {err}");
        Status::INTERNAL
    })?;
    Ok(mmio.into_split_send())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_test_metadata::{IntMetadata, Metadata};
    use fuchsia_async::Task;
    use futures_util::TryStreamExt;
    use mmio::Mmio;
    use std::collections::HashMap;
    use zx::{Vmo, VmoOp};

    struct TestServer {
        mmios: Vec<(&'static str, Option<pdev_fidl::Mmio>)>,
        metadata: HashMap<&'static str, Vec<u8>>,
    }

    impl TestServer {
        fn new() -> Self {
            Self { mmios: Vec::new(), metadata: HashMap::new() }
        }

        fn append_mmio(&mut self, name: &'static str, vmo: Vmo, offset: usize, size: usize) {
            self.mmios.push((
                name,
                Some(pdev_fidl::Mmio {
                    offset: Some(offset as u64),
                    size: Some(size as u64),
                    vmo: Some(vmo),
                    ..Default::default()
                }),
            ));
        }

        fn set_typed_metadata<T: Persistable + Serializable>(&mut self, metadata: &T) {
            let bytes = fidl::persist(metadata).unwrap();
            self.metadata.insert(T::SERIALIZABLE_NAME, bytes);
        }

        async fn handle_requests(
            &mut self,
            mut requests: pdev_fidl::DeviceRequestStream,
        ) -> Result<(), fidl::Error> {
            while let Some(req) = requests.try_next().await? {
                match req {
                    pdev_fidl::DeviceRequest::GetMmioById { index, responder } => {
                        responder.send(self.take_mmio_by_id(index).map_err(Status::into_raw))?;
                    }
                    pdev_fidl::DeviceRequest::GetMmioByName { name, responder } => {
                        responder.send(self.take_mmio_by_name(&name).map_err(Status::into_raw))?;
                    }
                    pdev_fidl::DeviceRequest::GetMetadata { id, responder } => {
                        responder.send(self.get_metadata(&id).map_err(Status::into_raw))?;
                    }
                    _ => {
                        unreachable!("not used by tests")
                    }
                }
            }
            Ok(())
        }

        fn take_mmio_by_id(&mut self, id: u32) -> Result<pdev_fidl::Mmio, Status> {
            self.mmios
                .get_mut(id as usize)
                .ok_or(Status::NOT_FOUND)?
                .1
                .take()
                .ok_or(Status::ALREADY_BOUND)
        }

        fn take_mmio_by_name(&mut self, name: &str) -> Result<pdev_fidl::Mmio, Status> {
            self.mmios
                .iter_mut()
                .find(|(n, _)| *n == name)
                .ok_or(Status::NOT_FOUND)?
                .1
                .take()
                .ok_or(Status::ALREADY_BOUND)
        }

        fn get_metadata(&self, id: &str) -> Result<&[u8], Status> {
            self.metadata.get(id).map(|v| v.as_slice()).ok_or(Status::NOT_FOUND)
        }

        fn run(mut self) -> (pdev_fidl::DeviceProxy, Task<Result<(), fidl::Error>>) {
            let (proxy, stream) =
                fidl::endpoints::create_proxy_and_stream::<pdev_fidl::DeviceMarker>();
            let server = Task::local(async move { self.handle_requests(stream).await });
            (proxy, server)
        }
    }

    #[fuchsia::test]
    async fn test_pdev() {
        let mut server = TestServer::new();

        let vmo = Vmo::create(4096).unwrap();
        vmo.op_range(VmoOp::ZERO, 0, 4096).unwrap();
        server.append_mmio("zero", vmo, 0, 4096);

        // Prepare the MMIO region.
        let vmo = Vmo::create(1024).unwrap();
        for i in 0..256 {
            vmo.write(&((i as u32).to_le_bytes()), (i * size_of::<u32>()) as u64).unwrap();
        }
        server.append_mmio("dev", vmo, 32 * size_of::<u32>(), 16);

        server.set_typed_metadata(&Metadata {
            test_field: Some("foo".to_string()),
            ..Default::default()
        });

        let (client, server) = server.run();

        let mmio = client.map_mmio_by_id(1).await.unwrap();
        assert_eq!(client.map_mmio_by_id(1).await.err(), Some(Status::ALREADY_BOUND));
        assert_eq!(client.map_mmio_by_id(2).await.err(), Some(Status::NOT_FOUND));
        assert_eq!(client.map_mmio_by_name("dev").await.err(), Some(Status::ALREADY_BOUND));

        assert_eq!(mmio.load32(0), 32);

        let mmio = client.map_mmio_by_name("zero").await.unwrap();
        assert_eq!(mmio.len(), 4096);
        assert_eq!(mmio.load64(128), 0);

        assert_eq!(
            client.get_typed_metadata::<Metadata>().await.unwrap(),
            Metadata { test_field: Some("foo".to_string()), ..Default::default() }
        );

        assert_eq!(client.get_typed_metadata::<IntMetadata>().await.err(), Some(Status::NOT_FOUND));

        let _ = server.abort().await;
    }
}
