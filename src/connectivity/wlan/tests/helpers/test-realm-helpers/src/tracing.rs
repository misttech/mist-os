// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::Proxy,
    fuchsia_component::client::connect_to_protocol_at,
    fuchsia_sync::Mutex,
    fuchsia_zircon::{self as zx, prelude::*},
    futures::AsyncReadExt,
    std::{io::Write, sync::Arc},
    tracing::info,
};

/// An RAII-style struct that initializes tracing in the test realm on creation via
/// `Tracing::create_and_initialize_tracing` and collects and writes the trace when the
/// struct is dropped.
/// The idea is that we can add this to different structs that represent different topologies and
/// get tracing.
pub struct Tracing {
    tracing_controller: Arc<Mutex<fidl_fuchsia_tracing_controller::ControllerSynchronousProxy>>,
    tracing_collector: Arc<Mutex<Option<std::thread::JoinHandle<Vec<u8>>>>>,
}

impl Tracing {
    pub async fn create_and_initialize_tracing(test_ns_prefix: &str) -> Self {
        let tracing_controller = connect_to_protocol_at::<
            fidl_fuchsia_tracing_controller::ControllerMarker,
        >(test_ns_prefix)
        .expect("Failed to get tracing controller.");
        let tracing_controller = fidl_fuchsia_tracing_controller::ControllerSynchronousProxy::new(
            fidl::Channel::from_handle(
                tracing_controller
                    .into_channel()
                    .expect("Failed to get fidl::AsyncChannel from proxy.")
                    .into_zx_channel()
                    .into_handle(),
            ),
        );
        let (tracing_socket, tracing_socket_write) = fidl::Socket::create_stream();
        tracing_controller
            .initialize_tracing(
                &fidl_fuchsia_tracing_controller::TraceConfig {
                    categories: Some(vec!["wlan".to_string()]),
                    buffer_size_megabytes_hint: Some(64),
                    ..Default::default()
                },
                tracing_socket_write,
            )
            .expect("Failed to initialize tracing.");

        let tracing_collector = std::thread::spawn(move || {
            let mut executor = fuchsia_async::LocalExecutor::new();
            executor.run_singlethreaded(async move {
                let mut tracing_socket = fuchsia_async::Socket::from_socket(tracing_socket);
                info!("draining trace record socket...");
                let mut buf = Vec::new();
                tracing_socket.read_to_end(&mut buf).await.unwrap();
                info!("trace record socket drained.");
                buf
            })
        });

        tracing_controller
            .start_tracing(
                &fidl_fuchsia_tracing_controller::StartOptions::default(),
                zx::Time::INFINITE,
            )
            .expect("Encountered FIDL error when starting trace.")
            .expect("Failed to start tracing.");

        let tracing_controller = Arc::new(Mutex::new(tracing_controller));
        let tracing_collector = Arc::new(Mutex::new(Some(tracing_collector)));

        let panic_hook = std::panic::take_hook();
        let tracing_controller_clone = Arc::clone(&tracing_controller);
        let tracing_collector_clone = Arc::clone(&tracing_collector);

        // Set a panic hook so a trace will be written upon panic. Even though we write a trace in the
        // destructor of TestHelper, we still must set this hook because Fuchsia uses the abort panic
        // strategy. If the unwind strategy were used, then all destructors would run and this hook would
        // not be necessary.
        std::panic::set_hook(Box::new(move |panic_info| {
            let tracing_controller = &mut tracing_controller_clone.lock();
            tracing_collector_clone.lock().take().map(move |tracing_collector| {
                Self::terminate_and_write_trace_(tracing_controller, tracing_collector);
            });
            panic_hook(panic_info);
        }));

        Self { tracing_controller, tracing_collector }
    }

    fn terminate_and_write_trace_(
        tracing_controller: &mut fidl_fuchsia_tracing_controller::ControllerSynchronousProxy,
        tracing_collector: std::thread::JoinHandle<Vec<u8>>,
    ) {
        // TODO: this doesn't seem to be stopping properly.
        // Terminate and write the trace before possibly panicking if WlantapPhy does
        // not shutdown gracefully.
        tracing_controller
            .terminate_tracing(
                &fidl_fuchsia_tracing_controller::TerminateOptions {
                    write_results: Some(true),
                    ..Default::default()
                },
                zx::Time::INFINITE,
            )
            .expect("Failed to stop tracing.");

        let trace = tracing_collector.join().expect("Failed to join tracing collector thread.");

        let fxt_path = format!("/custom_artifacts/trace.fxt");
        let mut fxt_file = std::fs::File::create(fxt_path).unwrap();
        fxt_file.write_all(&trace[..]).unwrap();
        fxt_file.sync_all().unwrap();
    }

    fn terminate_and_write_trace(&mut self) {
        Self::terminate_and_write_trace_(
            &mut self.tracing_controller.lock(),
            self.tracing_collector
                .lock()
                .take()
                .expect("Failed to acquire join handle for tracing collector."),
        );
    }
}

impl Drop for Tracing {
    fn drop(&mut self) {
        self.terminate_and_write_trace();
    }
}
