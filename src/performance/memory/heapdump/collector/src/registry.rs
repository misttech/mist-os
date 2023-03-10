// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_memory_heapdump_client::{self as fheapdump_client, CollectorError};
use fidl_fuchsia_memory_heapdump_process as fheapdump_process;
use fuchsia_zircon::Koid;
use futures::lock::Mutex;
use futures::StreamExt;
use std::collections::hash_map::{Entry, HashMap};
use std::sync::Arc;
use tracing::{info, warn};

use crate::process::Process;
use crate::process_v1::ProcessV1;

/// The "root" data structure, containing the state of the collector process.
pub struct Registry {
    // Registered live processes.
    processes: Mutex<HashMap<Koid, Arc<dyn Process>>>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry { processes: Mutex::new(HashMap::new()) }
    }

    async fn find_process_by_name(&self, name: &str) -> Result<Arc<dyn Process>, CollectorError> {
        let processes = self.processes.lock().await;
        let mut iterator = processes.values().filter(|p| p.get_name() == name);
        match (iterator.next(), iterator.next()) {
            (Some(process), None) => Ok(process.clone()),
            (None, _) => Err(CollectorError::ProcessSelectorNoMatch),
            (Some(_), Some(_)) => Err(CollectorError::ProcessSelectorAmbiguous),
        }
    }

    async fn find_process_by_koid(&self, koid: &Koid) -> Result<Arc<dyn Process>, CollectorError> {
        let result = self.processes.lock().await.get(koid).cloned();
        result.ok_or(CollectorError::ProcessSelectorNoMatch)
    }

    pub async fn serve_client_stream(
        &self,
        mut stream: fheapdump_client::CollectorRequestStream,
    ) -> Result<(), anyhow::Error> {
        while let Some(request) = stream.next().await.transpose()? {
            match request {
                fheapdump_client::CollectorRequest::TakeLiveSnapshot {
                    process_selector,
                    responder,
                } => {
                    let process = match process_selector {
                        fheapdump_client::ProcessSelector::ByName(name) => {
                            self.find_process_by_name(&name).await
                        }
                        fheapdump_client::ProcessSelector::ByKoid(koid) => {
                            self.find_process_by_koid(&Koid::from_raw(koid)).await
                        }
                        fheapdump_client::ProcessSelectorUnknown!() => {
                            warn!(ordinal = process_selector.ordinal(), "Unknown process selector");
                            Err(CollectorError::ProcessSelectorUnsupported)
                        }
                    };

                    match process {
                        Ok(_process) => {
                            // Not implemented yet, pretend that an error happened.
                            responder.send(&mut Err(CollectorError::LiveSnapshotFailed))?;
                        }
                        Err(e) => responder.send(&mut Err(e))?,
                    };
                }
            }
        }
        Ok(())
    }

    pub async fn serve_process_stream(
        &self,
        mut stream: fheapdump_process::RegistryRequestStream,
    ) -> Result<(), anyhow::Error> {
        let registration_request = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("No registration message was received"))??;

        let process: Arc<dyn Process> = match registration_request {
            fheapdump_process::RegistryRequest::RegisterV1 {
                process,
                allocations_vmo,
                snapshot_sink,
                ..
            } => {
                let process = ProcessV1::new(process, allocations_vmo, snapshot_sink)?;
                Arc::new(process)
            }
        };

        self.serve_process(process).await
    }

    async fn serve_process(&self, process: Arc<dyn Process>) -> Result<(), anyhow::Error> {
        let process_koid = process.get_koid();
        info!(koid = process_koid.raw_koid(), name = process.get_name(), "Process connected");
        match self.processes.lock().await.entry(process_koid) {
            Entry::Vacant(vacant_entry) => vacant_entry.insert(Arc::clone(&process)),
            Entry::Occupied(_) => {
                // This should not happen if the processes are well-behaved.
                anyhow::bail!("Another process with the same koid is already connected")
            }
        };

        let status = process.serve_until_exit().await;

        info!(koid = process_koid.raw_koid(), name = process.get_name(), "Process disconnected");
        self.processes.lock().await.remove(&process_koid).expect("Koid should still be present");

        // Propagate error only after removing the entry from `processes`.
        status
    }

    #[cfg(test)]
    pub async fn list_processes(&self) -> Vec<(Koid, String)> {
        self.processes
            .lock()
            .await
            .iter()
            .map(|(koid, process)| (*koid, process.get_name().to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use fidl::endpoints::create_proxy_and_stream;
    use fuchsia_async as fasync;
    use futures::{channel::oneshot, pin_mut};
    use test_case::test_case;

    fn create_registry_and_proxy(
        initial_processes: impl IntoIterator<Item = Arc<dyn Process>>,
    ) -> (Arc<Registry>, fheapdump_client::CollectorProxy) {
        // Create a new Registry and register the given processes.
        let registry = Arc::new(Registry::new());
        {
            let mut processes = registry.processes.try_lock().unwrap();
            for process in initial_processes {
                let koid = process.get_koid();
                let insert_result = processes.insert(koid, process);
                assert!(insert_result.is_none(), "found duplicate koid in initial_processes");
            }
        }

        // Create a client and start serving its stream from a detached task.
        let proxy = {
            let registry = registry.clone();
            let (proxy, stream) =
                create_proxy_and_stream::<fheapdump_client::CollectorMarker>().unwrap();

            let worker_fn = async move { registry.serve_client_stream(stream).await.unwrap() };
            fasync::Task::local(worker_fn).detach();

            proxy
        };

        (registry, proxy)
    }

    struct FakeProcess {
        name: String,
        koid: Koid,
        exit_signal: Mutex<Option<oneshot::Receiver<Result<(), anyhow::Error>>>>,
    }

    impl FakeProcess {
        /// Returns a new FakeProcess and a oneshot channel to make it exit with a given result.
        pub fn new(
            name: &str,
            koid: Koid,
        ) -> (Arc<dyn Process>, oneshot::Sender<anyhow::Result<()>>) {
            let (sender, receiver) = oneshot::channel();
            let fake_process = FakeProcess {
                name: name.to_string(),
                koid,
                exit_signal: Mutex::new(Some(receiver)),
            };
            (Arc::new(fake_process), sender)
        }
    }

    #[async_trait]
    impl Process for FakeProcess {
        fn get_name(&self) -> &str {
            &self.name
        }

        fn get_koid(&self) -> Koid {
            self.koid
        }

        async fn serve_until_exit(&self) -> Result<(), anyhow::Error> {
            let exit_signal = self.exit_signal.lock().await.take().unwrap();
            exit_signal.await?
        }
    }

    #[test_case(Ok(()) ; "exit ok")]
    #[test_case(Err(anyhow::anyhow!("Simulated error")) ; "exit error")]
    fn test_register_and_unregister(exit_result: Result<(), anyhow::Error>) {
        let mut ex = fasync::TestExecutor::new();
        let registry = Registry::new();

        // Setup fake process and register it.
        let name = "fake";
        let koid = Koid::from_raw(1234);
        let (process, signal) = FakeProcess::new(name, koid);
        let serve_fut = registry.serve_process(process);
        pin_mut!(serve_fut);
        assert!(ex.run_until_stalled(&mut serve_fut).is_pending());

        // Verify that the registry now contains the process.
        assert_eq!(ex.run_singlethreaded(registry.list_processes()), [(koid, name.to_string())]);

        // Simulate process exit.
        signal.send(exit_result).unwrap();
        assert!(ex.run_until_stalled(&mut serve_fut).is_ready());

        // Verify that the registry no longer contains the process.
        assert_eq!(ex.run_singlethreaded(registry.list_processes()), []);
    }

    #[test]
    fn test_cannot_register_same_koid_twice() {
        let mut ex = fasync::TestExecutor::new();
        let registry = Registry::new();

        // Create two FakeProcess instances with the same koid.
        let name1 = "fake-1";
        let name2 = "fake-2";
        let koid = Koid::from_raw(1234);
        let (process1, _signal1) = FakeProcess::new(name1, koid);
        let (process2, _signal2) = FakeProcess::new(name2, koid);

        // Register the first process.
        let serve1_fut = registry.serve_process(process1);
        pin_mut!(serve1_fut);
        assert!(ex.run_until_stalled(&mut serve1_fut).is_pending());

        // Verify that the registry now contains the process.
        assert_eq!(ex.run_singlethreaded(registry.list_processes()), [(koid, name1.to_string())]);

        // Verify that the second process cannot be registered:
        // - serve_process should exit immediately
        // - list_process_koids should not list the koid twice
        let serve2_fut = registry.serve_process(process2);
        pin_mut!(serve2_fut);
        assert!(ex.run_until_stalled(&mut serve2_fut).is_ready());
        assert_eq!(ex.run_singlethreaded(registry.list_processes()), [(koid, name1.to_string())]);

        // Verify that the first process stayed registered as if nothing happened.
        assert!(ex.run_until_stalled(&mut serve1_fut).is_pending());
    }

    #[test_case(fheapdump_client::ProcessSelector::ByName("foo".to_string()),
        CollectorError::LiveSnapshotFailed ; "valid name, snapshot fails")]
    #[test_case(fheapdump_client::ProcessSelector::ByName("bar".to_string()),
        CollectorError::ProcessSelectorAmbiguous ; "ambiguous name")]
    #[test_case(fheapdump_client::ProcessSelector::ByName("baz".to_string()),
        CollectorError::ProcessSelectorNoMatch ; "no matching name")]
    #[test_case(fheapdump_client::ProcessSelector::ByKoid(2),
        CollectorError::LiveSnapshotFailed ; "valid koid, snapshot fails")]
    #[test_case(fheapdump_client::ProcessSelector::ByKoid(99),
        CollectorError::ProcessSelectorNoMatch ; "no matching koid")]
    #[fasync::run_singlethreaded(test)]
    async fn test_take_live_snapshot(
        mut process_selector: fheapdump_client::ProcessSelector,
        expect_error: CollectorError,
    ) {
        // Create three FakeProcess instances, two of which with the same name.
        let (process1, _signal1) = FakeProcess::new("foo", Koid::from_raw(1));
        let (process2, _signal2) = FakeProcess::new("bar", Koid::from_raw(2));
        let (process3, _signal3) = FakeProcess::new("bar", Koid::from_raw(3));

        // Create a Registry and a client connected to it.
        let (_registry, proxy) = create_registry_and_proxy([process1, process2, process3]);

        // Execute the request.
        let result =
            proxy.take_live_snapshot(&mut process_selector).await.expect("FIDL channel error");

        assert_eq!(result.expect_err("request should fail"), expect_error);
    }
}
