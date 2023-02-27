// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_heapdump_process as fheapdump_process;
use fuchsia_zircon::{self as zx, AsHandleRef, Koid};
use futures::{lock::Mutex, StreamExt};

use crate::process::Process;

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
}
