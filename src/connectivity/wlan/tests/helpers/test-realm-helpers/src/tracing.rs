// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context};
use fidl::endpoints::Proxy;
use fuchsia_component::client::connect_to_protocol_at;
use fuchsia_sync::Mutex;
use zx::prelude::*;

use futures::AsyncReadExt;
use log::{info, warn};
use std::io::Write;
use std::sync::Arc;

/// An RAII-style struct that initializes tracing in the test realm on creation via
/// `Tracing::create_and_initialize_tracing` and collects and writes the trace when the
/// struct is dropped.
/// The idea is that we can add this to different structs that represent different topologies and
/// get tracing.
pub struct Tracing {
    trace_session: Arc<Mutex<Option<fidl_fuchsia_tracing_controller::SessionSynchronousProxy>>>,
    tracing_collector: Arc<Mutex<Option<std::thread::JoinHandle<Result<Vec<u8>, anyhow::Error>>>>>,
}

impl Tracing {
    pub async fn create_and_initialize_tracing(
        test_ns_prefix: &str,
    ) -> Result<Self, anyhow::Error> {
        let launcher =
            connect_to_protocol_at::<fidl_fuchsia_tracing_controller::ProvisionerMarker>(
                test_ns_prefix,
            )
            .map_err(|e| format_err!("Failed to get tracing controller: {e:?}"))?;
        let launcher = fidl_fuchsia_tracing_controller::ProvisionerSynchronousProxy::new(
            fidl::Channel::from_handle(
                launcher
                    .into_channel()
                    .map_err(|e| format_err!("Failed to get fidl::AsyncChannel from proxy: {e:?}"))?
                    .into_zx_channel()
                    .into_handle(),
            ),
        );
        let (tracing_socket, tracing_socket_write) = fidl::Socket::create_stream();
        let (trace_session, server) =
            fidl::endpoints::create_sync_proxy::<fidl_fuchsia_tracing_controller::SessionMarker>();
        launcher
            .initialize_tracing(
                server,
                &fidl_fuchsia_tracing_controller::TraceConfig {
                    categories: Some(vec!["wlan".to_string()]),
                    buffer_size_megabytes_hint: Some(64),
                    ..Default::default()
                },
                tracing_socket_write,
            )
            .map_err(|e| format_err!("Failed to initialize tracing: {e:?}"))?;

        let tracing_collector = std::thread::spawn(move || {
            let mut executor = fuchsia_async::LocalExecutor::new();
            executor.run_singlethreaded(async move {
                let mut tracing_socket = fuchsia_async::Socket::from_socket(tracing_socket);
                info!("draining trace record socket...");
                let mut buf = Vec::new();
                tracing_socket
                    .read_to_end(&mut buf)
                    .await
                    .map_err(|e| format_err!("Failed to drain trace record socket: {e:?}"))?;
                info!("trace record socket drained.");
                Ok(buf)
            })
        });

        trace_session
            .start_tracing(
                &fidl_fuchsia_tracing_controller::StartOptions::default(),
                zx::MonotonicInstant::INFINITE,
            )
            .map_err(|e| format_err!("Encountered FIDL error when starting trace: {e:?}"))?
            .map_err(|e| format_err!("Failed to start tracing: {e:?}"))?;

        let trace_session = Arc::new(Mutex::new(Some(trace_session)));
        let tracing_collector = Arc::new(Mutex::new(Some(tracing_collector)));

        let panic_hook = std::panic::take_hook();
        let trace_session_clone = Arc::clone(&trace_session);
        let tracing_collector_clone = Arc::clone(&tracing_collector);

        // Set a panic hook so a trace will be written upon panic. Even though we write a trace in the
        // destructor of TestHelper, we still must set this hook because Fuchsia uses the abort panic
        // strategy. If the unwind strategy were used, then all destructors would run and this hook would
        // not be necessary.
        std::panic::set_hook(Box::new(move |panic_info| {
            let trace_session = trace_session_clone.lock().take().expect("No trace collector");
            tracing_collector_clone.lock().take().map(move |tracing_collector| {
                let _: Result<(), ()> =
                    Self::terminate_and_write_trace_(trace_session, tracing_collector)
                        .map_err(|e| warn!("{e:?}"));
            });
            panic_hook(panic_info);
        }));

        Ok(Self { trace_session, tracing_collector })
    }

    fn terminate_and_write_trace_(
        trace_session: fidl_fuchsia_tracing_controller::SessionSynchronousProxy,
        tracing_collector: std::thread::JoinHandle<Result<Vec<u8>, anyhow::Error>>,
    ) -> Result<(), anyhow::Error> {
        // TODO: this doesn't seem to be stopping properly.
        // Terminate and write the trace before possibly panicking if WlantapPhy does
        // not shutdown gracefully.
        let _ = trace_session
            .stop_tracing(
                &fidl_fuchsia_tracing_controller::StopOptions {
                    write_results: Some(true),
                    ..Default::default()
                },
                zx::MonotonicInstant::INFINITE,
            )
            .map_err(|e| format_err!("Failed to stop tracing: {:?}", e));

        let trace = tracing_collector
            .join()
            .map_err(|e| format_err!("Failed to join tracing collector thread: {e:?}"))?
            .context(format_err!("Failed to collect trace."))?;

        let fxt_path = format!("/custom_artifacts/trace.fxt");
        let mut fxt_file = std::fs::File::create(&fxt_path)
            .map_err(|e| format_err!("Failed to create {}: {e:?}", &fxt_path))?;
        fxt_file
            .write_all(&trace[..])
            .map_err(|e| format_err!("Failed to write to {}: {e:?}", &fxt_path))?;
        fxt_file.sync_all().map_err(|e| format_err!("Failed to sync to {}: {e:?}", &fxt_path))?;
        Ok(())
    }

    fn terminate_and_write_trace(&mut self) {
        let mut trace_controller_mx = self.trace_session.lock();
        let trace_controller = trace_controller_mx.take().expect("No controller available");
        *trace_controller_mx = None;
        let _: Result<(), ()> = Self::terminate_and_write_trace_(
            trace_controller,
            self.tracing_collector
                .lock()
                .take()
                .expect("Failed to acquire join handle for tracing collector."),
        )
        .map_err(|e| warn!("{e:?}"));
    }
}

impl Drop for Tracing {
    fn drop(&mut self) {
        self.terminate_and_write_trace();
    }
}
