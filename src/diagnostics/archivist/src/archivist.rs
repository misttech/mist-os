// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::accessor::{ArchiveAccessorServer, BatchRetrievalTimeout};
use crate::component_lifecycle;
use crate::error::Error;
use crate::events::router::{ConsumerConfig, EventRouter, ProducerConfig};
use crate::events::sources::EventSource;
use crate::events::types::*;
use crate::identity::ComponentIdentity;
use crate::inspect::container::InspectHandle;
use crate::inspect::repository::InspectRepository;
use crate::inspect::servers::*;
use crate::logs::debuglog::KernelDebugLog;
use crate::logs::repository::{ComponentInitialInterest, LogsRepository};
use crate::logs::serial::{SerialConfig, SerialSink};
use crate::logs::servers::*;
use crate::pipeline::Pipeline;
use archivist_config::Config;
use fidl_fuchsia_diagnostics::ArchiveAccessorRequestStream;
use fidl_fuchsia_process_lifecycle::LifecycleRequestStream;
use fuchsia_component::server::{ServiceFs, ServiceObj};
use fuchsia_inspect::component;
use fuchsia_inspect::health::Reporter;
use futures::prelude::*;
use moniker::ExtendedMoniker;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use {fidl_fuchsia_diagnostics_host as fhost, fidl_fuchsia_io as fio, fuchsia_async as fasync};

/// Responsible for initializing an `Archivist` instance. Supports multiple configurations by
/// either calling or not calling methods on the builder like `serve_test_controller_protocol`.
pub struct Archivist {
    /// Handles event routing between archivist parts.
    event_router: EventRouter,

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

    /// The server handling fuchsia.diagnostics.LogStream
    log_stream_server: Arc<LogStreamServer>,

    /// The server handling fuchsia.inspect.InspectSink
    _inspect_sink_server: Arc<InspectSinkServer>,

    /// Top level scope.
    general_scope: fasync::Scope,

    /// Tasks receiving external events from component manager.
    incoming_events_scope: fasync::Scope,

    /// All tasks for FIDL servers that ingest data into the Archivist must run in this scope.
    servers_scope: fasync::Scope,
}

impl Archivist {
    /// Creates new instance, sets up inspect and adds 'archive' directory to output folder.
    /// Also installs `fuchsia.diagnostics.Archive` service.
    /// Call `install_log_services`
    pub async fn new(config: Config) -> Self {
        let general_scope = fasync::Scope::new();
        let servers_scope = fasync::Scope::new();

        // Initialize the pipelines that the archivist will expose.
        let pipelines = Self::init_pipelines(&config);

        // Initialize the core event router
        let mut event_router =
            EventRouter::new(component::inspector().root().create_child("events"));
        let incoming_events_scope = general_scope.new_child();
        Self::initialize_external_event_sources(&mut event_router, &incoming_events_scope).await;

        let initial_interests =
            config.component_initial_interests.into_iter().filter_map(|interest| {
                ComponentInitialInterest::from_str(&interest)
                    .map_err(|err| {
                        warn!(?err, invalid = %interest, "Failed to load initial interest");
                    })
                    .ok()
            });
        let logs_repo = LogsRepository::new(
            config.logs_max_cached_original_bytes,
            initial_interests,
            component::inspector().root(),
            general_scope.new_child(),
        );
        if !config.allow_serial_logs.is_empty() {
            let write_logs_to_serial =
                SerialConfig::new(config.allow_serial_logs, config.deny_serial_log_tags)
                    .write_logs(Arc::clone(&logs_repo), SerialSink);
            general_scope.spawn(write_logs_to_serial);
        }
        let inspect_repo = Arc::new(InspectRepository::new(
            pipelines.iter().map(Arc::downgrade).collect(),
            general_scope.new_child(),
        ));

        let inspect_sink_server =
            Arc::new(InspectSinkServer::new(Arc::clone(&inspect_repo), servers_scope.new_child()));

        // Initialize our FIDL servers. This doesn't start serving yet.
        let accessor_server = Arc::new(ArchiveAccessorServer::new(
            Arc::clone(&inspect_repo),
            Arc::clone(&logs_repo),
            config.maximum_concurrent_snapshots_per_reader,
            BatchRetrievalTimeout::from_seconds(config.per_component_batch_timeout_seconds),
            servers_scope.new_child(),
        ));

        let log_server =
            Arc::new(LogServer::new(Arc::clone(&logs_repo), servers_scope.new_child()));
        let log_stream_server =
            Arc::new(LogStreamServer::new(Arc::clone(&logs_repo), servers_scope.new_child()));

        // Initialize the external event providers containing incoming diagnostics directories and
        // log sink connections.
        event_router.add_consumer(ConsumerConfig {
            consumer: &logs_repo,
            events: vec![EventType::LogSinkRequested],
        });
        event_router.add_consumer(ConsumerConfig {
            consumer: &inspect_sink_server,
            events: vec![EventType::InspectSinkRequested],
        });

        // Drain klog and publish it to syslog.
        if config.enable_klog {
            match KernelDebugLog::new().await {
                Ok(klog) => logs_repo.drain_debuglog(klog),
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

        // TODO(https://fxbug.dev/324494668): remove this when Netstack2 is gone.
        if let Ok(dir) =
            fuchsia_fs::directory::open_in_namespace("/netstack-diagnostics", fio::PERM_READABLE)
        {
            inspect_repo.add_inspect_handle(
                Arc::new(ComponentIdentity::new(
                    ExtendedMoniker::parse_str("core/network/netstack").unwrap(),
                    "fuchsia-pkg://fuchsia.com/netstack#meta/netstack2.cm",
                )),
                InspectHandle::directory(dir),
            );
        }

        Self {
            accessor_server,
            log_server,
            log_stream_server,
            event_router,
            _inspect_sink_server: inspect_sink_server,
            pipelines,
            _inspect_repository: inspect_repo,
            logs_repository: logs_repo,
            general_scope,
            servers_scope,
            incoming_events_scope,
        }
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
        scope: &fasync::Scope,
    ) {
        match EventSource::new("/events/log_sink_requested_event_stream").await {
            Err(err) => warn!(?err, "Failed to create event source for log sink requests"),
            Ok(mut event_source) => {
                event_router.add_producer(ProducerConfig {
                    producer: &mut event_source,
                    events: vec![EventType::LogSinkRequested],
                });
                scope.spawn(async move {
                    // This should never exit.
                    let _ = event_source.spawn().await;
                });
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
                scope.spawn(async move {
                    // This should never exit.
                    let _ = event_source.spawn().await;
                });
            }
        }
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
        is_embedded: bool,
        request_stream: LifecycleRequestStream,
    ) -> Result<(), Error> {
        debug!("Running Archivist.");

        // Start servicing all outgoing services.
        self.serve_protocols(&mut fs);
        let svc_task = self.general_scope.spawn(fs.collect::<()>());

        let _inspect_server_task = inspect_runtime::publish(
            component::inspector(),
            inspect_runtime::PublishOptions::default(),
        );

        let Self {
            pipelines: _pipelines,
            _inspect_repository,
            logs_repository,
            accessor_server: _accessor_server,
            log_server: _log_server,
            log_stream_server: _log_stream_server,
            _inspect_sink_server,
            general_scope,
            incoming_events_scope,
            servers_scope,
            event_router,
        } = self;

        // Start ingesting events.
        let (terminate_handle, drain_events_fut) = event_router
            .start()
            // panic: can only panic if we didn't register event producers and consumers correctly.
            .expect("Failed to start event router");
        general_scope.spawn(drain_events_fut);

        let servers_scope_handle = servers_scope.to_handle();
        general_scope.spawn(component_lifecycle::on_stop_request(request_stream, || async move {
            terminate_handle.terminate().await;
            debug!("Stopped ingesting new CapabilityRequested events");
            incoming_events_scope.cancel().await;
            debug!("Cancel all tasks currently executing in our event router");
            servers_scope_handle.close();
            logs_repository.stop_accepting_new_log_sinks();
            debug!("Close any new connections to FIDL servers");
            svc_task.cancel().await;
            debug!("Stop allowing new connections through the incoming namespace.");
            logs_repository.wait_for_termination().await;
            debug!("All LogSink connections have finished");
            servers_scope.join().await;
            debug!("All servers stopped.");
        }));

        if is_embedded {
            debug!("Entering core loop.");
        } else {
            info!("archivist: Entering core loop.");
        }

        general_scope.await;

        Ok(())
    }

    fn serve_protocols(&mut self, fs: &mut ServiceFs<ServiceObj<'static, ()>>) {
        component::serve_inspect_stats();

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
            // TODO(https://fxbug.dev/42077091): Add Inspect support
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

        // Server fuchsia.logger.LogStream
        let log_stream_server = Arc::clone(&self.log_stream_server);
        svc_dir.add_fidl_service(move |stream| {
            debug!("fuchsia.logger.LogStream connection");
            log_stream_server.spawn(stream);
        });

        // Server fuchsia.diagnostics.LogSettings
        let log_settings_server = LogSettingsServer::new(
            Arc::clone(&self.logs_repository),
            // Don't create this in the servers scope. We don't care about this protocol for
            // shutdown purposes.
            self.general_scope.new_child(),
        );
        svc_dir.add_fidl_service(move |stream| {
            debug!("fuchsia.diagnostics.LogSettings connection");
            log_settings_server.spawn(stream);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::*;
    use crate::events::router::{Dispatcher, EventProducer};
    use crate::logs::testing::*;
    use diagnostics_data::LogsData;
    use fidl::endpoints::create_proxy;
    use fidl_fuchsia_diagnostics::{
        ClientSelectorConfiguration, DataType, Format, StreamParameters,
    };
    use fidl_fuchsia_inspect::{InspectSinkMarker, InspectSinkRequestStream};
    use fidl_fuchsia_logger::{LogSinkMarker, LogSinkRequestStream};
    use fidl_fuchsia_process_lifecycle::{LifecycleMarker, LifecycleProxy};
    use fuchsia_component::client::connect_to_protocol_at_dir_svc;
    use futures::channel::oneshot;
    use std::marker::PhantomData;
    use {fidl_fuchsia_diagnostics_host as fhost, fidl_fuchsia_io as fio, fuchsia_async as fasync};

    async fn init_archivist(fs: &mut ServiceFs<ServiceObj<'static, ()>>) -> Archivist {
        let config = Config {
            enable_klog: false,
            log_to_debuglog: false,
            maximum_concurrent_snapshots_per_reader: 4,
            logs_max_cached_original_bytes: LEGACY_DEFAULT_MAXIMUM_CACHED_LOGS_BYTES,
            num_threads: 1,
            pipelines_path: DEFAULT_PIPELINES_PATH.into(),
            bind_services: vec![],
            allow_serial_logs: vec![],
            deny_serial_log_tags: vec![],
            component_initial_interests: vec![],
            per_component_batch_timeout_seconds: -1,
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
            events: vec![EventType::InspectSinkRequested],
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
                    timestamp: zx::BootInstant::get(),
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
                    timestamp: zx::BootInstant::get(),
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
        let archivist = init_archivist(&mut fs).await;
        let (signal_send, signal_recv) = oneshot::channel();
        let (lifecycle_proxy, request_stream) =
            fidl::endpoints::create_proxy_and_stream::<LifecycleMarker>().unwrap();
        fasync::Task::spawn(async move {
            archivist.run(fs, false, request_stream).await.expect("Cannot run archivist");
            signal_send.send(()).unwrap();
        })
        .detach();
        (directory, lifecycle_proxy, signal_recv)
    }

    // runs archivist and returns its directory.
    async fn run_archivist() -> (fio::DirectoryProxy, LifecycleProxy) {
        let (directory, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
        let mut fs = ServiceFs::new();
        fs.serve_connection(server_end).unwrap();
        let archivist = init_archivist(&mut fs).await;
        let (lifecycle_proxy, request_stream) =
            fidl::endpoints::create_proxy_and_stream::<LifecycleMarker>().unwrap();
        fasync::Task::spawn(async move {
            archivist.run(fs, false, request_stream).await.expect("Cannot run archivist");
        })
        .detach();
        (directory, lifecycle_proxy)
    }

    #[fuchsia::test]
    async fn can_log_and_retrive_log() {
        let (directory, _proxy) = run_archivist().await;
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
        let (directory, _proxy) = run_archivist().await;
        let accessor =
            connect_to_protocol_at_dir_svc::<fhost::ArchiveAccessorMarker>(&directory).unwrap();
        loop {
            let (local, remote) = zx::Socket::create_stream();
            let mut reader = fuchsia_async::Socket::from_socket(local);
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
        let (directory, _proxy) = run_archivist().await;
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
