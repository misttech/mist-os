// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_heapdump_process as fheapdump_process;
use fuchsia_zircon::Koid;
use futures::lock::Mutex;
use futures::StreamExt;
use std::collections::hash_map::{Entry, HashMap};
use std::sync::Arc;
use tracing::info;

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
    use fuchsia_async as fasync;
    use futures::{channel::oneshot, pin_mut};
    use test_case::test_case;

    struct FakeProcess {
        name: String,
        koid: Koid,
        exit_signal: Mutex<Option<oneshot::Receiver<Result<(), anyhow::Error>>>>,
    }

    impl FakeProcess {
        /// Returns a new FakeProcess and a oneshot channel to make it exit with a given result.
        pub fn new(name: &str, koid: Koid) -> (FakeProcess, oneshot::Sender<anyhow::Result<()>>) {
            let (sender, receiver) = oneshot::channel();
            let fake_process = FakeProcess {
                name: name.to_string(),
                koid,
                exit_signal: Mutex::new(Some(receiver)),
            };
            (fake_process, sender)
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
        let serve_fut = registry.serve_process(Arc::new(process));
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
        let serve1_fut = registry.serve_process(Arc::new(process1));
        pin_mut!(serve1_fut);
        assert!(ex.run_until_stalled(&mut serve1_fut).is_pending());

        // Verify that the registry now contains the process.
        assert_eq!(ex.run_singlethreaded(registry.list_processes()), [(koid, name1.to_string())]);

        // Verify that the second process cannot be registered:
        // - serve_process should exit immediately
        // - list_process_koids should not list the koid twice
        let serve2_fut = registry.serve_process(Arc::new(process2));
        pin_mut!(serve2_fut);
        assert!(ex.run_until_stalled(&mut serve2_fut).is_ready());
        assert_eq!(ex.run_singlethreaded(registry.list_processes()), [(koid, name1.to_string())]);

        // Verify that the first process stayed registered as if nothing happened.
        assert!(ex.run_until_stalled(&mut serve1_fut).is_pending());
    }
}
