// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_memory_heapdump_client as fheapdump_client;
use fidl_fuchsia_memory_heapdump_process as fheapdump_process;
use fuchsia_zircon::{self as zx, AsHandleRef, Koid};
use futures::{lock::Mutex, StreamExt};
use vmo_types::allocations_table_v1::AllocationsTableReader;

use crate::process::{Process, Snapshot};

fn get_process_name(process: &zx::Process) -> Result<String, anyhow::Error> {
    Ok(String::from_utf8_lossy(process.get_name()?.as_bytes()).to_string())
}

/// An instrumented process that speaks version 1 of the protocol.
///
/// In particular, version 1 of the protocol implies the usage of:
/// - `allocations_table_v1` for the `allocations_vmo`
// TODO(fdurso): Remove this allow(dead_code) once the features that use all these fields are
// implemented.
#[allow(dead_code)]
pub struct ProcessV1 {
    koid: Koid,
    name: String,
    process: zx::Process,
    allocations_vmo: zx::Vmo,
    snapshot_stream: Mutex<fheapdump_process::SnapshotSinkV1RequestStream>,
}

impl ProcessV1 {
    pub fn new(
        process: zx::Process,
        allocations_vmo: zx::Vmo,
        snapshot_sink: ServerEnd<fheapdump_process::SnapshotSinkV1Marker>,
    ) -> Result<ProcessV1, anyhow::Error> {
        let name = get_process_name(&process)?;
        let koid = process.get_koid()?;
        Ok(ProcessV1 {
            name,
            koid,
            process,
            allocations_vmo,
            snapshot_stream: Mutex::new(snapshot_sink.into_stream()?),
        })
    }
}

#[async_trait]
impl Process for ProcessV1 {
    fn get_name(&self) -> &str {
        &self.name
    }

    fn get_koid(&self) -> Koid {
        self.koid
    }

    async fn serve_until_exit(&self) -> Result<(), anyhow::Error> {
        let mut stream = self.snapshot_stream.lock().await;
        while let Some(request) = stream.next().await.transpose()? {
            match request {
                // TODO(fxbug.dev/122389): Application-initiated snapshots are not implemented yet.
            }
        }

        Ok(())
    }

    fn take_live_snapshot(&self) -> Result<Box<dyn Snapshot>, anyhow::Error> {
        let size = self.allocations_vmo.get_size()?;
        let snapshot = self.allocations_vmo.create_child(zx::VmoChildOptions::SNAPSHOT, 0, size)?;
        Ok(Box::new(SnapshotV1::new(snapshot)))
    }
}

/// A snapshot of a V1 process.
pub struct SnapshotV1 {
    allocations_vmo: zx::Vmo,
}

impl SnapshotV1 {
    pub fn new(allocations_vmo: zx::Vmo) -> SnapshotV1 {
        SnapshotV1 { allocations_vmo }
    }
}

#[async_trait]
impl Snapshot for SnapshotV1 {
    async fn write_to(
        &self,
        dest: fheapdump_client::SnapshotReceiverProxy,
    ) -> Result<(), anyhow::Error> {
        let mut streamer = heapdump_snapshot::Streamer::new(dest);
        let allocations_table = AllocationsTableReader::new(&self.allocations_vmo)?;

        for block in allocations_table.iter() {
            let block = block?;
            streamer = streamer
                .push_element(fheapdump_client::SnapshotElement::Allocation(
                    fheapdump_client::Allocation {
                        address: Some(block.address),
                        size: Some(block.size),
                        ..fheapdump_client::Allocation::EMPTY
                    },
                ))
                .await?;
        }

        Ok(streamer.end_of_stream().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::{create_proxy, create_proxy_and_stream};
    use fuchsia_async as fasync;
    use futures::pin_mut;
    use vmo_types::allocations_table_v1::AllocationsTableWriter;

    use crate::registry::Registry;

    /// Creates an empty allocation VMO and ties it to the current process in the given registry.
    ///
    /// By reusing the current process, instead of creating a new one, we avoid requiring the
    /// ZX_POL_NEW_PROCESS capability.
    fn setup_fake_process_from_self() -> (
        fheapdump_process::RegistryRequestStream,
        fheapdump_process::SnapshotSinkV1Proxy,
        Koid,
        AllocationsTableWriter,
    ) {
        // Create and initialize the VMO.
        const VMO_SIZE: u64 = 1 << 31;
        let allocations_vmo = zx::Vmo::create(VMO_SIZE).unwrap();
        let allocations_writer = AllocationsTableWriter::new(&allocations_vmo).unwrap();

        let process = fuchsia_runtime::process_self().duplicate(zx::Rights::SAME_RIGHTS).unwrap();
        let koid = process.get_koid().unwrap();

        // Create channels and send the registration message.
        let (registry_proxy, registry_stream) =
            create_proxy_and_stream::<fheapdump_process::RegistryMarker>().unwrap();
        let (snapshot_proxy, snapshot_server) =
            create_proxy::<fheapdump_process::SnapshotSinkV1Marker>().unwrap();
        registry_proxy.register_v1(process, allocations_vmo, snapshot_server).unwrap();

        (registry_stream, snapshot_proxy, koid, allocations_writer)
    }

    #[test]
    fn test_register_and_unregister() {
        let mut ex = fasync::TestExecutor::new();
        let registry = Registry::new();

        // Setup fake process.
        let (registry_stream, snapshot_sink, koid, _) = setup_fake_process_from_self();
        let name = get_process_name(&fuchsia_runtime::process_self()).unwrap();

        // Register it.
        let serve_fut = registry.serve_process_stream(registry_stream);
        pin_mut!(serve_fut);
        assert!(ex.run_until_stalled(&mut serve_fut).is_pending());

        // Verify that the registry now contains the process.
        assert_eq!(ex.run_singlethreaded(registry.list_processes()), [(koid, name)]);

        // Simulate process exit by dropping the snapshot sink channel.
        std::mem::drop(snapshot_sink);
        assert!(ex.run_until_stalled(&mut serve_fut).is_ready());

        // Verify that the registry no longer contains the process.
        assert_eq!(ex.run_singlethreaded(registry.list_processes()), []);
    }

    #[test]
    fn test_take_live_snapshot() {
        let mut ex = fasync::TestExecutor::new();
        let registry = std::rc::Rc::new(Registry::new());

        // Setup fake process.
        let (registry_stream, _snapshot_sink, koid, mut allocations_writer) =
            setup_fake_process_from_self();

        // Register it.
        let serve_fut = registry.serve_process_stream(registry_stream);
        pin_mut!(serve_fut);
        assert!(ex.run_until_stalled(&mut serve_fut).is_pending());

        // Record an allocation.
        let foobar = Box::pin(b"foobar");
        let foobar_address = foobar.as_ptr() as u64;
        let foobar_size = foobar.len() as u64;
        allocations_writer.insert_allocation(foobar_address, foobar_size).unwrap();

        // Take a live snapshot.
        let mut received_snapshot = ex.run_singlethreaded(async {
            let (receiver_proxy, receiver_stream) =
                create_proxy_and_stream::<fheapdump_client::SnapshotReceiverMarker>().unwrap();
            let receive_worker =
                fasync::Task::local(heapdump_snapshot::Snapshot::receive_from(receiver_stream));

            let process = registry.get_process(&koid).await.unwrap();
            let snapshot = process.take_live_snapshot().unwrap();
            snapshot.write_to(receiver_proxy).await.expect("failed to write snapshot");

            receive_worker.await.expect("failed to receive snapshot")
        });

        // Verify that it contains our test allocation.
        let foobar_allocation = received_snapshot.allocations.remove(&foobar_address).unwrap();
        assert_eq!(foobar_allocation.size, foobar_size);

        assert!(received_snapshot.allocations.is_empty(), "all the entries have been removed");
    }
}
