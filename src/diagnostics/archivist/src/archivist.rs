// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    accessor::ArchiveAccessorServer,
    component_lifecycle,
    error::Error,
    events::{
        router::{ConsumerConfig, EventRouter, ProducerConfig, RouterOptions},
        sources::{ComponentEventProvider, EventSource, LogConnector},
        types::*,
    },
    inspect::{repository::InspectRepository, servers::*},
    logs::{
        repository::LogsRepository,
        serial::{SerialConfig, SerialSink},
        servers::*,
        KernelDebugLog,
    },
    pipeline::Pipeline,
};
use archivist_config::Config;
use fidl_fuchsia_diagnostics::ArchiveAccessorRequestStream;
use fidl_fuchsia_diagnostics_host as fhost;
use fidl_fuchsia_process_lifecycle::LifecycleRequestStream;
use fidl_fuchsia_sys_internal as fsys_internal;
use fuchsia_async as fasync;
use fuchsia_component::{
    client::connect_to_protocol,
    server::{ServiceFs, ServiceObj},
};
use fuchsia_inspect::{component, health::Reporter};
use fuchsia_zircon as zx;
use futures::{
    channel::oneshot,
    future::{self, abortable},
    prelude::*,
};
use std::{path::Path, sync::Arc};
use tracing::{debug, error, info, warn};

/// Responsible for initializing an `Archivist` instance. Supports multiple configurations by
/// either calling or not calling methods on the builder like `serve_test_controller_protocol`.
pub struct Archivist {
    /// Handles event routing between archivist parts.
    event_router: EventRouter,

    /// Receive stop signal to kill this archivist.
    stop_recv: Option<oneshot::Receiver<()>>,

    /// Listens for lifecycle requests, to handle Stop requests.
    lifecycle_task: Option<fasync::Task<()>>,

    /// Tasks that drains klog.
    _drain_klog_task: Option<fasync::Task<()>>,

    /// Task writing logs to serial.
    _serial_task: Option<fasync::Task<()>>,

    /// Tasks receiving external events from component manager and appmgr.
    incoming_external_event_producers: Vec<fasync::Task<()>>,

    /// The diagnostics pipelines that have been installed.
    pipelines: Vec<Arc<Pipeline>>,

    /// The repository holding Inspect data.
    _inspect_repository: Arc<InspectRepository>,

    /// The repository holding active log connections.
    logs_repository: Arc<LogsRepository>,

    /// The server handling fuchsia.diagnostics.ArchiveAccessor
    accessor_server: Arc<ArchiveAccessorServer>,

    /// The server handling fuchsia.logger.Log
    log_server: Arc<LogServer>,

    /// The server handling fuchsia.inspect.InspectSink
    inspect_server: Arc<InspectSinkServer>,

    /// The server handling fuchsia.diagnostics.LogSettings
    log_settings_server: Arc<LogSettingsServer>,
}

impl Archivist {
    /// Creates new instance, sets up inspect and adds 'archive' directory to output folder.
    /// Also installs `fuchsia.diagnostics.Archive` service.
    /// Call `install_log_services`
    pub async fn new(config: Config) -> Self {
        // Initialize the pipelines that the archivist will expose.
        let pipelines = Self::init_pipelines(&config);

        // Initialize the core event router
        let mut event_router =
            EventRouter::new(component::inspector().root().create_child("events"));
        let incoming_external_event_producers =
            Self::initialize_external_event_sources(&mut event_router, &config).await;

        let logs_repo = LogsRepository::new(
            config.logs_max_cached_original_bytes,
            component::inspector().root(),
        )
        .await;
        let serial_task = if !config.allow_serial_logs.is_empty() {
            Some(fasync::Task::spawn(
                SerialConfig::new(config.allow_serial_logs, config.deny_serial_log_tags)
                    .write_logs(Arc::clone(&logs_repo), SerialSink::default()),
            ))
        } else {
            None
        };
        let inspect_repo =
            Arc::new(InspectRepository::new(pipelines.iter().map(Arc::downgrade).collect()));

        let inspect_server = Arc::new(InspectSinkServer::new(Arc::clone(&inspect_repo)));

        // Initialize our FIDL servers. This doesn't start serving yet.
        let accessor_server = Arc::new(ArchiveAccessorServer::new(
            Arc::clone(&inspect_repo),
            Arc::clone(&logs_repo),
            config.maximum_concurrent_snapshots_per_reader,
        ));

        let log_server = Arc::new(LogServer::new(Arc::clone(&logs_repo)));
        let log_settings_server = Arc::new(LogSettingsServer::new(Arc::clone(&logs_repo)));

        // Initialize the external event providers containing incoming diagnostics directories and
        // log sink connections.
        event_router.add_consumer(ConsumerConfig {
            consumer: &logs_repo,
            events: vec![EventType::LogSinkRequested],
        });
        event_router.add_consumer(ConsumerConfig {
            consumer: &inspect_repo,
            events: vec![EventType::DiagnosticsReady],
        });
        event_router.add_consumer(ConsumerConfig {
            consumer: &inspect_server,
            events: vec![EventType::InspectSinkRequested],
        });

        // Drain klog and publish it to syslog.
        if config.enable_klog {
            match KernelDebugLog::new().await {
                Ok(klog) => logs_repo.drain_debuglog(klog).await,
                Err(err) => warn!(
                    ?err,
                    "Failed to start the kernel debug log reader. Klog won't be in syslog"
                ),
            };
        }

        // Start related services that should start once the Archivist has started.
        for name in &config.bind_services {
            info!("Connecting to service {}", name);
            let (_local, remote) = zx::Channel::create();
            if let Err(e) = fdio::service_connect(&format!("/svc/{name}"), remote) {
                error!("Couldn't connect to service {}: {:?}", name, e);
            }
        }

        Self {
            accessor_server,
            log_server,
            inspect_server,
            log_settings_server,
            event_router,
            _serial_task: serial_task,
            stop_recv: None,
            lifecycle_task: None,
            _drain_klog_task: None,
            incoming_external_event_producers,
            pipelines,
            _inspect_repository: inspect_repo,
            logs_repository: logs_repo,
        }
    }

    /// Sets the request stream from which Lifecycle/Stop requests will come instructing the
    /// Archivist to stop ingesting new data and drain current data to clients.
    pub fn set_lifecycle_request_stream(&mut self, request_stream: LifecycleRequestStream) {
        debug!("Lifecycle listener initialized.");
        let (t, r) = component_lifecycle::serve_v2(request_stream);
        self.lifecycle_task = Some(t);
        self.stop_recv = Some(r);
    }

    fn init_pipelines(config: &Config) -> Vec<Arc<Pipeline>> {
        let pipelines_node = component::inspector().root().create_child("pipelines");
        let accessor_stats_node =
            component::inspector().root().create_child("archive_accessor_stats");
        let pipelines_path = Path::new(&config.pipelines_path);
        let pipelines = [
            Pipeline::feedback(pipelines_path, &pipelines_node, &accessor_stats_node),
            Pipeline::legacy_metrics(pipelines_path, &pipelines_node, &accessor_stats_node),
            Pipeline::lowpan(pipelines_path, &pipelines_node, &accessor_stats_node),
            Pipeline::all_access(pipelines_path, &pipelines_node, &accessor_stats_node),
        ];

        if pipelines.iter().any(|p| p.config_has_error()) {
            component::health().set_unhealthy("Pipeline config has an error");
        } else {
            component::health().set_ok();
        }
        let pipelines = pipelines.into_iter().map(Arc::new).collect::<Vec<_>>();

        component::inspector().root().record(pipelines_node);
        component::inspector().root().record(accessor_stats_node);

        pipelines
    }

    pub async fn initialize_external_event_sources(
        event_router: &mut EventRouter,
        config: &Config,
    ) -> Vec<fasync::Task<()>> {
        let mut incoming_external_event_producers = vec![];
        if config.enable_component_event_provider {
            match connect_to_protocol::<fsys_internal::ComponentEventProviderMarker>() {
                Err(err) => {
                    warn!(?err, "Failed to connect to fuchsia.sys.internal.ComponentEventProvider");
                }
                Ok(proxy) => {
                    let mut component_event_provider = ComponentEventProvider::new(proxy);
                    event_router.add_producer(ProducerConfig {
                        producer: &mut component_event_provider,
                        events: vec![EventType::DiagnosticsReady],
                    });
                    incoming_external_event_producers.push(fasync::Task::spawn(async move {
                        component_event_provider.spawn().await.unwrap_or_else(|err| {
                            warn!(?err, "Failed to run component event provider loop");
                        });
                    }));
                }
            };
        }

        if config.enable_event_source {
            match EventSource::new("/events/log_sink_requested_event_stream").await {
                Err(err) => warn!(?err, "Failed to create event source for log sink requests"),
                Ok(mut event_source) => {
                    event_router.add_producer(ProducerConfig {
                        producer: &mut event_source,
                        events: vec![EventType::LogSinkRequested],
                    });
                    incoming_external_event_producers.push(fasync::Task::spawn(async move {
                        // This should never exit.
                        let _ = event_source.spawn().await;
                    }));
                }
            }

            match EventSource::new("/events/inspect_sink_requested_event_stream").await {
                Err(err) => {
                    warn!(?err, "Failed to create event source for InspectSink requests")
                }
                Ok(mut event_source) => {
                    event_router.add_producer(ProducerConfig {
                        producer: &mut event_source,
                        events: vec![EventType::InspectSinkRequested],
                    });
                    incoming_external_event_producers.push(fasync::Task::spawn(async move {
                        // This should never exit.
                        let _ = event_source.spawn().await;
                    }));
                }
            }

            match EventSource::new("/events/diagnostics_ready_event_stream").await {
                Err(err) => {
                    warn!(?err, "Failed to create event source for diagnostics ready requests")
                }
                Ok(mut event_source) => {
                    event_router.add_producer(ProducerConfig {
                        producer: &mut event_source,
                        events: vec![EventType::DiagnosticsReady],
                    });
                    incoming_external_event_producers.push(fasync::Task::spawn(async move {
                        // This should never exit.
                        let _ = event_source.spawn().await;
                    }));
                }
            }
        }

        if config.enable_log_connector {
            match connect_to_protocol::<fsys_internal::LogConnectorMarker>() {
                Err(err) => {
                    warn!(?err, "Failed to connect to fuchsia.sys.internal.LogConnector");
                }
                Ok(proxy) => {
                    let mut connector = LogConnector::new(proxy);
                    event_router.add_producer(ProducerConfig {
                        producer: &mut connector,
                        events: vec![EventType::LogSinkRequested],
                    });
                    incoming_external_event_producers.push(fasync::Task::spawn(async move {
                        connector.spawn().await.unwrap_or_else(|err| {
                            warn!(?err, "Failed to run event source producer loop");
                        });
                    }));
                }
            }
        }

        incoming_external_event_producers
    }

    fn add_host_before_last_dot(input: &str) -> String {
        let (rest, last) = input.rsplit_once('.').unwrap();
        format!("{}.host.{}", rest, last)
    }

    /// Run archivist to completion.
    /// # Arguments:
    /// * `outgoing_channel`- channel to serve outgoing directory on.
    pub async fn run(
        mut self,
        mut fs: ServiceFs<ServiceObj<'static, ()>>,
        router_opts: RouterOptions,
    ) -> Result<(), Error> {
        debug!("Running Archivist.");

        // Start servicing all outgoing services.
        self.serve_protocols(&mut fs).await;
        let run_outgoing = fs.collect::<()>();

        // Start ingesting events.
        let (terminate_handle, drain_events_fut) = self
            .event_router
            .start(router_opts)
            // panic: can only panic if we didn't register event producers and consumers correctly.
            .expect("Failed to start event router");
        let _event_routing_task = fasync::Task::spawn(async move {
            drain_events_fut.await;
        });

        let accessor_server = Arc::clone(&self.accessor_server);
        let log_server = Arc::clone(&self.log_server);
        let logs_repo = Arc::clone(&self.logs_repository);
        let inspect_server = Arc::clone(&self.inspect_server);
        let all_msg = async {
            logs_repo.wait_for_termination().await;
            debug!("Flushing to listeners.");
            accessor_server.wait_for_servers_to_complete().await;
            log_server.wait_for_servers_to_complete().await;
            debug!("Log listeners and batch iterators stopped.");
            inspect_server.wait_for_servers_to_complete().await;
        };

        let (abortable_fut, abort_handle) = abortable(run_outgoing);

        let log_server = self.log_server;
        let inspect_server = self.inspect_server;
        let accessor_server = self.accessor_server;
        let incoming_external_event_producers = self.incoming_external_event_producers;
        let logs_repo = Arc::clone(&self.logs_repository);
        let stop_fut = match self.stop_recv {
            Some(stop_recv) => async move {
                stop_recv.into_future().await.ok();
                terminate_handle.terminate().await;
                for task in incoming_external_event_producers {
                    task.cancel().await;
                }
                future::join(log_server.stop(), inspect_server.stop()).await;
                accessor_server.stop().await;
                logs_repo.stop_accepting_new_log_sinks().await;
                abort_handle.abort()
            }
            .left_future(),
            None => future::ready(()).right_future(),
        };

        // Ensure logs repo remains alive since it holds BudgetManager which
        // should remain alive.
        let _logs_repo = self.logs_repository;

        info!("archivist: Entering core loop.");
        // Combine all three futures into a main future.
        future::join3(abortable_fut, stop_fut, all_msg).map(|_| Ok(())).await
    }

    async fn serve_protocols(&mut self, fs: &mut ServiceFs<ServiceObj<'static, ()>>) {
        component::serve_inspect_stats();
        inspect_runtime::serve(component::inspector(), fs)
            .unwrap_or_else(|err| warn!(?err, "failed to serve diagnostics"));

        let mut svc_dir = fs.dir("svc");

        // Serve fuchsia.diagnostics.ArchiveAccessors backed by a pipeline.
        for pipeline in &self.pipelines {
            let host_accessor_server = Arc::clone(&self.accessor_server);
            let accessor_server = Arc::clone(&self.accessor_server);
            let accessor_pipeline = Arc::clone(pipeline);
            svc_dir.add_fidl_service_at(
                pipeline.protocol_name(),
                move |stream: ArchiveAccessorRequestStream| {
                    accessor_server.spawn_server(Arc::clone(&accessor_pipeline), stream);
                },
            );
            let accessor_pipeline = Arc::clone(pipeline);
            // TODO(https://fxbug.dev/126321): Add Inspect support
            let accessor = Self::add_host_before_last_dot(accessor_pipeline.protocol_name());
            svc_dir.add_fidl_service_at(
                accessor,
                move |stream: fhost::ArchiveAccessorRequestStream| {
                    host_accessor_server.spawn_server(Arc::clone(&accessor_pipeline), stream);
                },
            );
        }

        // Server fuchsia.logger.Log
        let log_server = Arc::clone(&self.log_server);
        svc_dir.add_fidl_service(move |stream| {
            debug!("fuchsia.logger.Log connection");
            log_server.spawn(stream);
        });

        // Server fuchsia.diagnostics.LogSettings
        let log_settings_server = Arc::clone(&self.log_settings_server);
        svc_dir.add_fidl_service(move |stream| {
            debug!("fuchsia.diagnostics.LogSettings connection");
            log_settings_server.spawn(stream);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        constants::*,
        events::router::{Dispatcher, EventProducer},
        identity::ComponentIdentity,
        logs::testing::*,
    };
    use diagnostics_data::LogsData;
    use fidl::endpoints::create_proxy;
    use fidl_fuchsia_diagnostics::{
        ClientSelectorConfiguration, DataType, Format, StreamParameters,
    };
    use fidl_fuchsia_diagnostics_host as fhost;
    use fidl_fuchsia_inspect::{InspectSinkMarker, InspectSinkRequestStream};
    use fidl_fuchsia_io as fio;
    use fidl_fuchsia_logger::{LogSinkMarker, LogSinkRequestStream};
    use fidl_fuchsia_process_lifecycle::{LifecycleMarker, LifecycleProxy};
    use fuchsia_async as fasync;
    use fuchsia_component::client::connect_to_protocol_at_dir_svc;
    use futures::channel::oneshot;
    use std::marker::PhantomData;

    async fn init_archivist(fs: &mut ServiceFs<ServiceObj<'static, ()>>) -> Archivist {
        let config = Config {
            enable_component_event_provider: false,
            enable_klog: false,
            enable_event_source: false,
            enable_log_connector: false,
            log_to_debuglog: false,
            maximum_concurrent_snapshots_per_reader: 4,
            logs_max_cached_original_bytes: LEGACY_DEFAULT_MAXIMUM_CACHED_LOGS_BYTES,
            num_threads: 1,
            pipelines_path: DEFAULT_PIPELINES_PATH.into(),
            bind_services: vec![],
            allow_serial_logs: vec![],
            deny_serial_log_tags: vec![],
        };

        let mut archivist = Archivist::new(config).await;

        // Install a couple of iunattributed sources for the purposes of the test.
        let mut source = UnattributedSource::<LogSinkMarker>::default();
        archivist.event_router.add_producer(ProducerConfig {
            producer: &mut source,
            events: vec![EventType::LogSinkRequested],
        });
        fs.dir("svc").add_fidl_service(move |stream| {
            source.new_connection(stream);
        });

        let mut source = UnattributedSource::<InspectSinkMarker>::default();
        archivist.event_router.add_producer(ProducerConfig {
            producer: &mut source,
            events: vec![
                EventType::InspectSinkRequested,
                // This producer doesn't generate this event, but we can use it to satisfy the
                // validation.
                EventType::DiagnosticsReady,
            ],
        });
        fs.dir("svc").add_fidl_service(move |stream| {
            source.new_connection(stream);
        });

        archivist
    }

    pub struct UnattributedSource<P> {
        dispatcher: Dispatcher,
        _phantom: PhantomData<P>,
    }

    impl<P> Default for UnattributedSource<P> {
        fn default() -> Self {
            Self { dispatcher: Dispatcher::default(), _phantom: PhantomData }
        }
    }

    impl UnattributedSource<LogSinkMarker> {
        pub fn new_connection(&mut self, request_stream: LogSinkRequestStream) {
            self.dispatcher
                .emit(Event {
                    timestamp: zx::Time::get_monotonic(),
                    payload: EventPayload::LogSinkRequested(LogSinkRequestedPayload {
                        component: Arc::new(ComponentIdentity::unknown()),
                        request_stream,
                    }),
                })
                .ok();
        }
    }

    impl UnattributedSource<InspectSinkMarker> {
        pub fn new_connection(&mut self, request_stream: InspectSinkRequestStream) {
            self.dispatcher
                .emit(Event {
                    timestamp: zx::Time::get_monotonic(),
                    payload: EventPayload::InspectSinkRequested(InspectSinkRequestedPayload {
                        component: Arc::new(ComponentIdentity::unknown()),
                        request_stream,
                    }),
                })
                .ok();
        }
    }

    impl<P> EventProducer for UnattributedSource<P> {
        fn set_dispatcher(&mut self, dispatcher: Dispatcher) {
            self.dispatcher = dispatcher;
        }
    }

    // run archivist and send signal when it dies.
    async fn run_archivist_and_signal_on_exit(
    ) -> (fio::DirectoryProxy, LifecycleProxy, oneshot::Receiver<()>) {
        let (directory, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();

        let mut fs = ServiceFs::new();
        fs.serve_connection(server_end).unwrap();
        let mut archivist = init_archivist(&mut fs).await;

        let (lifecycle_proxy, request_stream) =
            fidl::endpoints::create_proxy_and_stream::<LifecycleMarker>().unwrap();
        archivist.set_lifecycle_request_stream(request_stream);
        let (signal_send, signal_recv) = oneshot::channel();
        fasync::Task::spawn(async move {
            archivist.run(fs, RouterOptions::default()).await.expect("Cannot run archivist");
            signal_send.send(()).unwrap();
        })
        .detach();
        (directory, lifecycle_proxy, signal_recv)
    }

    // runs archivist and returns its directory.
    async fn run_archivist() -> fio::DirectoryProxy {
        let (directory, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
        let mut fs = ServiceFs::new();
        fs.serve_connection(server_end).unwrap();
        let archivist = init_archivist(&mut fs).await;
        fasync::Task::spawn(async move {
            archivist.run(fs, RouterOptions::default()).await.expect("Cannot run archivist");
        })
        .detach();
        directory
    }

    #[fuchsia::test]
    async fn can_log_and_retrive_log() {
        let directory = run_archivist().await;
        let mut recv_logs = start_listener(&directory);

        let mut log_helper = LogSinkHelper::new(&directory);
        log_helper.write_log("my msg1");
        log_helper.write_log("my msg2");

        assert_eq!(
            vec! {Some("my msg1".to_owned()),Some("my msg2".to_owned())},
            vec! {recv_logs.next().await,recv_logs.next().await}
        );

        // new client can log
        let mut log_helper2 = LogSinkHelper::new(&directory);
        log_helper2.write_log("my msg1");
        log_helper.write_log("my msg2");

        let mut expected = vec!["my msg1".to_owned(), "my msg2".to_owned()];
        expected.sort();

        let mut actual = vec![recv_logs.next().await.unwrap(), recv_logs.next().await.unwrap()];
        actual.sort();

        assert_eq!(expected, actual);

        // can log after killing log sink proxy
        log_helper.kill_log_sink();
        log_helper.write_log("my msg1");
        log_helper.write_log("my msg2");

        assert_eq!(
            expected,
            vec! {recv_logs.next().await.unwrap(),recv_logs.next().await.unwrap()}
        );

        // can log from new socket cnonnection
        log_helper2.add_new_connection();
        log_helper2.write_log("my msg1");
        log_helper2.write_log("my msg2");

        assert_eq!(
            expected,
            vec! {recv_logs.next().await.unwrap(),recv_logs.next().await.unwrap()}
        );
    }

    #[fuchsia::test]
    async fn remote_log_test() {
        let directory = run_archivist().await;
        let accessor =
            connect_to_protocol_at_dir_svc::<fhost::ArchiveAccessorMarker>(&directory).unwrap();
        loop {
            let (local, remote) = fuchsia_zircon::Socket::create_stream();
            let mut reader = fuchsia_async::Socket::from_socket(local).unwrap();
            accessor
                .stream_diagnostics(
                    &StreamParameters {
                        data_type: Some(DataType::Logs),
                        stream_mode: Some(fidl_fuchsia_diagnostics::StreamMode::Snapshot),
                        format: Some(Format::Json),
                        client_selector_configuration: Some(
                            ClientSelectorConfiguration::SelectAll(true),
                        ),
                        ..Default::default()
                    },
                    remote,
                )
                .await
                .unwrap();
            let log_helper = LogSinkHelper::new(&directory);
            let log_writer = log_helper.connect();
            LogSinkHelper::write_log_at(&log_writer, "Test message");
            let mut data = vec![];
            reader.read_to_end(&mut data).await.unwrap();
            if data.is_empty() {
                continue;
            }
            let logs = serde_json::from_slice::<Vec<LogsData>>(&data).unwrap();
            for log in logs {
                if log.msg() == Some("Test message") {
                    return;
                }
            }
        }
    }

    /// Makes sure that implementation can handle multiple sockets from same
    /// log sink.
    #[fuchsia::test]
    async fn log_from_multiple_sock() {
        let directory = run_archivist().await;
        let mut recv_logs = start_listener(&directory);

        let log_helper = LogSinkHelper::new(&directory);
        let sock1 = log_helper.connect();
        let sock2 = log_helper.connect();
        let sock3 = log_helper.connect();

        LogSinkHelper::write_log_at(&sock1, "msg sock1-1");
        LogSinkHelper::write_log_at(&sock2, "msg sock2-1");
        LogSinkHelper::write_log_at(&sock1, "msg sock1-2");
        LogSinkHelper::write_log_at(&sock3, "msg sock3-1");
        LogSinkHelper::write_log_at(&sock2, "msg sock2-2");

        let mut expected = vec![
            "msg sock1-1".to_owned(),
            "msg sock1-2".to_owned(),
            "msg sock2-1".to_owned(),
            "msg sock2-2".to_owned(),
            "msg sock3-1".to_owned(),
        ];
        expected.sort();

        let mut actual = vec![
            recv_logs.next().await.unwrap(),
            recv_logs.next().await.unwrap(),
            recv_logs.next().await.unwrap(),
            recv_logs.next().await.unwrap(),
            recv_logs.next().await.unwrap(),
        ];
        actual.sort();

        assert_eq!(expected, actual);
    }

    /// Stop API works
    #[fuchsia::test]
    async fn stop_works() {
        let (directory, lifecycle_proxy, signal_recv) = run_archivist_and_signal_on_exit().await;
        let mut recv_logs = start_listener(&directory);

        {
            // make sure we can write logs
            let log_sink_helper = LogSinkHelper::new(&directory);
            let sock1 = log_sink_helper.connect();
            LogSinkHelper::write_log_at(&sock1, "msg sock1-1");
            log_sink_helper.write_log("msg sock1-2");
            let mut expected = vec!["msg sock1-1".to_owned(), "msg sock1-2".to_owned()];
            expected.sort();
            let mut actual = vec![recv_logs.next().await.unwrap(), recv_logs.next().await.unwrap()];
            actual.sort();
            assert_eq!(expected, actual);

            //  Start new connections and sockets
            let log_sink_helper1 = LogSinkHelper::new(&directory);
            let sock2 = log_sink_helper.connect();
            // Write logs before calling stop
            log_sink_helper1.write_log("msg 1");
            log_sink_helper1.write_log("msg 2");
            let log_sink_helper2 = LogSinkHelper::new(&directory);

            lifecycle_proxy.stop().unwrap();

            // make more socket connections and write to them and old ones.
            let sock3 = log_sink_helper2.connect();
            log_sink_helper2.write_log("msg 3");
            log_sink_helper2.write_log("msg 4");

            LogSinkHelper::write_log_at(&sock3, "msg 5");
            LogSinkHelper::write_log_at(&sock2, "msg 6");
            log_sink_helper.write_log("msg 7");
            LogSinkHelper::write_log_at(&sock1, "msg 8");

            LogSinkHelper::write_log_at(&sock2, "msg 9");
        } // kills all sockets and log_sink connections
        let mut expected = vec![];
        let mut actual = vec![];
        for i in 1..=9 {
            expected.push(format!("msg {i}"));
            actual.push(recv_logs.next().await.unwrap());
        }
        expected.sort();
        actual.sort();

        // make sure archivist is dead.
        signal_recv.await.unwrap();

        assert_eq!(expected, actual);
    }
}
