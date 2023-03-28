// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_memory_heapdump_client as fheapdump_client;
use fidl_fuchsia_memory_heapdump_process as fheapdump_process;
use fuchsia_zircon::{self as zx, AsHandleRef, Koid};
use futures::{lock::Mutex, StreamExt};
use heapdump_vmo::{
    allocations_table_v1::AllocationsTableReader, resources_table_v1::ResourcesTableReader,
    stack_trace_compression,
};
use std::{collections::HashSet, sync::Arc};

use crate::process::{Process, Snapshot};
use crate::utils::find_executable_regions;

fn get_process_name(process: &zx::Process) -> Result<String, anyhow::Error> {
    Ok(String::from_utf8_lossy(process.get_name()?.as_bytes()).to_string())
}

/// An instrumented process that speaks version 1 of the protocol.
///
/// In particular, version 1 of the protocol implies the usage of:
/// - `allocations_table_v1` for the `allocations_vmo`
/// - `resources_table_v1` for the `resources_vmo`
pub struct ProcessV1 {
    koid: Koid,
    name: String,
    process: zx::Process,
    allocations_vmo: zx::Vmo,
    resources_vmo: Arc<zx::Vmo>,
    snapshot_stream: Mutex<fheapdump_process::SnapshotSinkV1RequestStream>,
}

impl ProcessV1 {
    pub fn new(
        process: zx::Process,
        allocations_vmo: zx::Vmo,
        resources_vmo: zx::Vmo,
        snapshot_sink: ServerEnd<fheapdump_process::SnapshotSinkV1Marker>,
    ) -> Result<ProcessV1, anyhow::Error> {
        let name = get_process_name(&process)?;
        let koid = process.get_koid()?;
        Ok(ProcessV1 {
            name,
            koid,
            process,
            allocations_vmo,
            resources_vmo: Arc::new(resources_vmo),
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
        let executable_regions = find_executable_regions(&self.process)?;
        Ok(Box::new(SnapshotV1::new(snapshot, self.resources_vmo.clone(), executable_regions)))
    }
}

/// A snapshot of a V1 process.
pub struct SnapshotV1 {
    allocations_vmo: zx::Vmo,
    resources_vmo: Arc<zx::Vmo>,
    executable_regions: Vec<fheapdump_client::ExecutableRegion>,
}

impl SnapshotV1 {
    pub fn new(
        allocations_vmo: zx::Vmo,
        resources_vmo: Arc<zx::Vmo>,
        executable_regions: Vec<fheapdump_client::ExecutableRegion>,
    ) -> SnapshotV1 {
        SnapshotV1 { allocations_vmo, resources_vmo, executable_regions }
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
        let resources_table = ResourcesTableReader::new(&self.resources_vmo)?;

        let mut stack_trace_keys = HashSet::new();
        for block in allocations_table.iter() {
            let block = block?;
            stack_trace_keys.insert(block.stack_trace_key);

            streamer = streamer
                .push_element(fheapdump_client::SnapshotElement::Allocation(
                    fheapdump_client::Allocation {
                        address: Some(block.address),
                        size: Some(block.size),
                        stack_trace_key: Some(block.stack_trace_key.into_raw() as u64),
                        ..fheapdump_client::Allocation::EMPTY
                    },
                ))
                .await?;
        }

        for stack_trace_key in stack_trace_keys {
            let compressed_stack_trace =
                resources_table.get_compressed_stack_trace(stack_trace_key)?;
            let uncompressed_stack_trace =
                stack_trace_compression::uncompress(compressed_stack_trace);

            // Split long stack traces in chunks so that they can be paginated.
            const MAX_ENTRIES_PER_CHUNK: usize = 32;
            for chunk in uncompressed_stack_trace.chunks(MAX_ENTRIES_PER_CHUNK) {
                streamer = streamer
                    .push_element(fheapdump_client::SnapshotElement::StackTrace(
                        fheapdump_client::StackTrace {
                            stack_trace_key: Some(stack_trace_key.into_raw() as u64),
                            program_addresses: Some(chunk.to_vec()),
                            ..fheapdump_client::StackTrace::EMPTY
                        },
                    ))
                    .await?;
            }
        }

        for executable_region in &self.executable_regions {
            streamer = streamer
                .push_element(fheapdump_client::SnapshotElement::ExecutableRegion(
                    executable_region.clone(),
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
    use heapdump_vmo::{
        allocations_table_v1::AllocationsTableWriter, resources_table_v1::ResourcesTableWriter,
        stack_trace_compression,
    };
    use itertools::{assert_equal, Itertools};
    use std::collections::HashMap;

    use crate::registry::Registry;

    /// Creates empty allocation and resource VMOs and ties them to the current process in the given
    /// registry.
    ///
    /// By reusing the current process, instead of creating a new one, we avoid requiring the
    /// ZX_POL_NEW_PROCESS capability.
    fn setup_fake_process_from_self() -> (
        fheapdump_process::RegistryRequestStream,
        fheapdump_process::SnapshotSinkV1Proxy,
        Koid,
        AllocationsTableWriter,
        ResourcesTableWriter,
    ) {
        // Create and initialize the VMOs.
        const VMO_SIZE: u64 = 1 << 31;
        let allocations_vmo = zx::Vmo::create(VMO_SIZE).unwrap();
        let allocations_writer = AllocationsTableWriter::new(&allocations_vmo).unwrap();
        let resources_vmo = zx::Vmo::create(VMO_SIZE).unwrap();
        let resources_writer = ResourcesTableWriter::new(&resources_vmo).unwrap();

        let process = fuchsia_runtime::process_self().duplicate(zx::Rights::SAME_RIGHTS).unwrap();
        let koid = process.get_koid().unwrap();

        // Create channels and send the registration message.
        let (registry_proxy, registry_stream) =
            create_proxy_and_stream::<fheapdump_process::RegistryMarker>().unwrap();
        let (snapshot_proxy, snapshot_server) =
            create_proxy::<fheapdump_process::SnapshotSinkV1Marker>().unwrap();
        registry_proxy
            .register_v1(process, allocations_vmo, resources_vmo, snapshot_server)
            .unwrap();

        (registry_stream, snapshot_proxy, koid, allocations_writer, resources_writer)
    }

    // Asserts that the given list of executable regions correctly describes the fake process.
    //
    // Note: the fake process (created by `setup_fake_process_from_self`) is actually the current
    // process. Therefore, this function compares the regions described in `actual` to the list of
    // the executable regions in the current process.
    fn assert_executable_regions_valid_for_fake_process(
        actual: &HashMap<u64, heapdump_snapshot::ExecutableRegion>,
    ) {
        // Enumerate the expected executable regions.
        let expected = find_executable_regions(&fuchsia_runtime::process_self())
            .unwrap()
            .into_iter()
            .map(|region| {
                (
                    region.address.unwrap(),
                    region.size.unwrap(),
                    region.file_offset.unwrap(),
                    region.build_id.unwrap().value,
                )
            });

        // Convert the actual regions to the same format, so that they can be compared.
        let actual = actual.iter().map(|(address, region)| {
            (*address, region.size, region.file_offset, region.build_id.clone())
        });

        // Assert that both iterators return the same elements.
        assert_equal(actual.sorted(), expected.sorted());
    }

    #[test]
    fn test_register_and_unregister() {
        let mut ex = fasync::TestExecutor::new();
        let registry = Registry::new();

        // Setup fake process.
        let (registry_stream, snapshot_sink, koid, _, _) = setup_fake_process_from_self();
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

    // Generate a very long stack trace to verify that the pagination mechanism works.
    fn generate_fake_long_stack_trace() -> Vec<u64> {
        // Ideally we would like to test with a stack trace whose uncompressed size is bigger than
        // ZX_CHANNEL_MAX_MSG_BYTES. However, given that stack trace compression has not been
        // implemented yet, ResourcesTableWriter will refuse to insert such stack traces.
        // This is the longest possible stack trace length that ResourcesTableWriter can store right
        // now.
        // TODO(fxbug.dev/123360): Raise this number to its maximum value when compression is
        // implemented.
        const NUM_FRAMES: usize = (u16::MAX as usize) / std::mem::size_of::<u64>();
        (0..NUM_FRAMES as u64).collect()
    }

    #[test]
    fn test_take_live_snapshot() {
        let mut ex = fasync::TestExecutor::new();
        let registry = std::rc::Rc::new(Registry::new());

        // Setup fake process.
        let (registry_stream, _snapshot_sink, koid, mut allocations_writer, mut resources_writer) =
            setup_fake_process_from_self();

        // Register it.
        let serve_fut = registry.serve_process_stream(registry_stream);
        pin_mut!(serve_fut);
        assert!(ex.run_until_stalled(&mut serve_fut).is_pending());

        // Record an allocation.
        let foobar = Box::pin(b"foobar");
        let foobar_address = foobar.as_ptr() as u64;
        let foobar_size = foobar.len() as u64;
        let foobar_stack_trace = generate_fake_long_stack_trace();
        let (foobar_stack_trace_key, _) = resources_writer
            .intern_compressed_stack_trace(&stack_trace_compression::compress(&foobar_stack_trace))
            .unwrap();
        allocations_writer
            .insert_allocation(foobar_address, foobar_size, foobar_stack_trace_key)
            .unwrap();

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
        assert_eq!(foobar_allocation.stack_trace.program_addresses, foobar_stack_trace);

        assert!(received_snapshot.allocations.is_empty(), "all the entries have been removed");

        // Verify that it contains the expected executable regions.
        assert_executable_regions_valid_for_fake_process(&received_snapshot.executable_regions);
    }
}
