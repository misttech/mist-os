// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This program serves the fuchsia.archivist.test.Puppet protocol.
//
// It is meant to be controlled by a test suite and will emit log messages
// and inspect data as requested. This output can be retrieved from the
// archivist under test using fuchsia.diagnostics.ArchiveAccessor.
//
// For full documentation, see //src/diagnostics/archivist/testing/realm-factory/README.md

use anyhow::{Context, Error, Result};
use diagnostics_hierarchy::Property;
use diagnostics_log::{OnInterestChanged, Publisher, PublisherOptions, TestRecord};
use diagnostics_log_encoding::Argument;
use fidl::endpoints::create_request_stream;
use fidl_table_validation::ValidFidlTable;
use fuchsia_async::{TaskGroup, Timer};
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::health::Reporter;
use fuchsia_inspect::{component, Inspector};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::lock::Mutex;
use futures::{FutureExt, StreamExt, TryStreamExt};
use inspect_runtime::EscrowOptions;
use inspect_testing::ExampleInspectData;
use std::sync::Arc;
use tracing::{debug, error, info, trace, warn};
use {fidl_fuchsia_archivist_test as fpuppet, fidl_fuchsia_diagnostics as fdiagnostics};

enum IncomingServices {
    Puppet(fpuppet::PuppetRequestStream),
    InspectPuppet(fpuppet::InspectPuppetRequestStream),
}

// `logging = false` allows us to set the global default trace dispatcher
// ourselves. This can only be done once and is usually handled by fuchsia::main.
#[fuchsia::main(logging = false)]
async fn main() -> Result<(), Error> {
    // Listen for log interest change events.
    let (interest_send, interest_recv) = unbounded::<InterestChangedEvent>();
    subscribe_to_log_interest_changes(InterestChangedNotifier(interest_send))?;

    let mut fs = ServiceFs::new();

    let puppet_server = Arc::new(PuppetServer::new(interest_recv));

    fs.dir("svc")
        .add_fidl_service(IncomingServices::Puppet)
        .add_fidl_service(IncomingServices::InspectPuppet);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, |service| async {
        match service {
            IncomingServices::Puppet(s) => {
                serve_puppet(puppet_server.clone(), s).await;
            }
            IncomingServices::InspectPuppet(s) => {
                serve_inspect_puppet(puppet_server.clone(), s).await;
            }
        }
    })
    .await;

    Ok(())
}

fn subscribe_to_log_interest_changes(notifier: InterestChangedNotifier) -> Result<(), Error> {
    // Don't wait for initial interest. Many times the test cases rely on knowing when the
    // component received its initial interest to know that it's running and already serving FIDL
    // requests.
    let publisher = Publisher::new(PublisherOptions::default().wait_for_initial_interest(false))?;
    publisher.set_interest_listener(notifier);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(%info, "PANIC");
        previous_hook(info);
    }));
    tracing::subscriber::set_global_default(publisher)?;
    Ok(())
}

#[derive(Clone)]
struct InterestChangedEvent {
    severity: fdiagnostics::Severity,
}

struct PuppetServer {
    // A stream of noifications about interest changed events.
    interest_changed: Mutex<UnboundedReceiver<InterestChangedEvent>>,
    // Tasks waiting to be notified of interest changed events.
    interest_waiters: Mutex<TaskGroup>,
    // Published Inspectors
    published_inspectors: Mutex<TaskGroup>,
}

impl PuppetServer {
    fn new(receiver: UnboundedReceiver<InterestChangedEvent>) -> Self {
        Self {
            interest_changed: Mutex::new(receiver),
            interest_waiters: Mutex::new(TaskGroup::new()),
            published_inspectors: Mutex::new(TaskGroup::new()),
        }
    }
}

// Notifies the puppet when log interest changes.
//
// Together, `PuppetServer` and `InterestChangeNotifier` must guarantee delivery
// of all interest change notifications to clients (test cases) regardless of
// whether a test case begins waiting for the interest change notification
// before or after it is received by this component. Failure to deliver will
// cause the test case to hang.
#[derive(Clone)]
struct InterestChangedNotifier(UnboundedSender<InterestChangedEvent>);

impl OnInterestChanged for InterestChangedNotifier {
    fn on_changed(&self, severity: diagnostics_log::Severity) {
        let sender = self.0.clone();
        // Panic on failure since undelivered notifications may hang clients.
        sender.unbounded_send(InterestChangedEvent { severity: severity.into() }).unwrap();
    }
}

async fn serve_puppet(server: Arc<PuppetServer>, mut stream: fpuppet::PuppetRequestStream) {
    while let Ok(Some(request)) = stream.try_next().await {
        handle_puppet_request(server.clone(), request)
            .await
            .unwrap_or_else(|e| error!(?e, "handle_puppet_request"));
    }
}

async fn serve_inspect_puppet(
    server: Arc<PuppetServer>,
    mut stream: fpuppet::InspectPuppetRequestStream,
) {
    while let Ok(Some(request)) = stream.try_next().await {
        handle_inspect_puppet_request(server.clone(), request)
            .await
            .unwrap_or_else(|e| error!(?e, "handle_puppet_request"));
    }
}

async fn handle_inspect_writer(
    mut stream: fpuppet::InspectWriterRequestStream,
    name: Option<String>,
) -> Result<(), Error> {
    let inspector = match name {
        None => component::inspector().clone(),
        Some(_) => Inspector::default(),
    };

    let opts = match name {
        None => inspect_runtime::PublishOptions::default(),
        Some(ref n) => inspect_runtime::PublishOptions::default().inspect_tree_name(n),
    };

    let controller = inspect_runtime::publish(&inspector, opts).unwrap();

    let mut example_inspect = ExampleInspectData::default();

    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fpuppet::InspectWriterRequest::EmitExampleInspectData { responder } => {
                example_inspect.write_to(inspector.root());
                responder.send().expect("response succeeds")
            }
            fpuppet::InspectWriterRequest::RecordString { key, value, responder } => {
                inspector.root().record_string(key, value);
                responder.send().expect("response succeeds")
            }
            fpuppet::InspectWriterRequest::RecordInt { key, value, responder } => {
                inspector.root().record_int(key, value);
                responder.send().expect("response succeeds")
            }
            fpuppet::InspectWriterRequest::SetHealthOk { responder } => {
                if name.is_none() {
                    component::health().set_ok();
                }

                responder.send().expect("response succeeds")
            }
            fpuppet::InspectWriterRequest::EscrowAndExit {
                payload: fpuppet::InspectWriterEscrowAndExitRequest { name, .. },
                responder,
            } => {
                let mut options = EscrowOptions::default();
                if let Some(name) = name {
                    options = options.name(name);
                }
                let token = controller.escrow_frozen(options).await.unwrap();
                responder
                    .send(fpuppet::InspectWriterEscrowAndExitResponse {
                        token: Some(token),
                        ..Default::default()
                    })
                    .expect("response succeeds");
                std::process::exit(0);
            }
            fpuppet::InspectWriterRequest::_UnknownMethod { .. } => unreachable!(),
        }
    }
    Ok(())
}

async fn handle_inspect_puppet_request(
    server: Arc<PuppetServer>,
    request: fpuppet::InspectPuppetRequest,
) -> Result<(), Error> {
    match request {
        fpuppet::InspectPuppetRequest::CreateInspector {
            payload: fpuppet::InspectPuppetCreateInspectorRequest { name, .. },
            responder,
        } => {
            let (client_end, server_end) = fidl::endpoints::create_endpoints();
            server.published_inspectors.lock().await.spawn(async move {
                handle_inspect_writer(server_end.into_stream().unwrap(), name).await.unwrap()
            });

            responder.send(client_end).expect("response succeeds");
        }
        fpuppet::InspectPuppetRequest::_UnknownMethod { .. } => unreachable!(),
    }
    Ok(())
}

async fn handle_puppet_request(
    server: Arc<PuppetServer>,
    request: fpuppet::PuppetRequest,
) -> Result<(), Error> {
    match request {
        fpuppet::PuppetRequest::CreateInspector {
            payload: fpuppet::InspectPuppetCreateInspectorRequest { name, .. },
            responder,
        } => {
            let (client_end, server_end) = fidl::endpoints::create_endpoints();
            server.published_inspectors.lock().await.spawn(async move {
                handle_inspect_writer(server_end.into_stream().unwrap(), name).await.unwrap()
            });

            responder.send(client_end).expect("response succeeds");
        }
        fpuppet::PuppetRequest::Crash { message, .. } => {
            panic!("{message}");
        }
        fpuppet::PuppetRequest::RecordLazyValues { key, responder } => {
            let (client, requests) = create_request_stream()?;
            responder.send(client).expect("response succeeds");
            record_lazy_values(key, requests).await?;
        }
        fpuppet::PuppetRequest::Println { message, responder } => {
            println!("{message}");
            responder.send().expect("response succeeds")
        }
        fpuppet::PuppetRequest::Eprintln { message, responder } => {
            eprintln!("{message}");
            responder.send().expect("response succeeds")
        }
        fpuppet::PuppetRequest::Log { payload, responder, .. } => {
            let request = LogRequest::try_from(payload).context("Invalid log")?;
            let LogRequest { message, severity, time, .. } = request;

            match time {
                None => match severity {
                    fdiagnostics::Severity::Trace => trace!("{message}"),
                    fdiagnostics::Severity::Debug => debug!("{message}"),
                    fdiagnostics::Severity::Info => info!("{message}"),
                    fdiagnostics::Severity::Warn => warn!("{message}"),
                    fdiagnostics::Severity::Error => error!("{message}"),
                    _ => unimplemented!("Logging with severity: {severity:?}"),
                },
                Some(time) => {
                    tracing::dispatcher::get_default(|dispatcher| {
                        let publisher: &diagnostics_log::Publisher =
                            dispatcher.downcast_ref().unwrap();
                        let record = TestRecord {
                            severity: severity.into_primitive(),
                            timestamp: zx::BootInstant::from_nanos(time),
                            file: None,
                            line: None,
                            record_arguments: vec![Argument::message(message.as_str())],
                        };
                        publisher.event_for_testing(record);
                    });
                }
            }
            responder.send().expect("response succeeds")
        }
        fpuppet::PuppetRequest::WaitForInterestChange { responder } => {
            let mut task_group = server.interest_waiters.lock().await;
            let server = server.clone();
            task_group.spawn(async move {
                let event = server.interest_changed.lock().await.next().await.unwrap();
                let response = &fpuppet::LogPuppetWaitForInterestChangeResponse {
                    severity: Some(event.severity),
                    ..Default::default()
                };
                responder.send(response).expect("response succeeds");
            });
        }
        fpuppet::PuppetRequest::_UnknownMethod { .. } => unreachable!(),
    }
    Ok(())
}

#[derive(Debug, Clone, ValidFidlTable)]
#[fidl_table_src(fpuppet::LogPuppetLogRequest)]
#[fidl_table_strict]
pub struct LogRequest {
    pub message: String,
    pub severity: fdiagnostics::Severity,
    #[fidl_field_type(optional)]
    pub time: Option<i64>,
}

// Converts InspectPuppet requests into callbacks that report inspect values lazily.
// The values aren't truly lazy since they're computed in the client before the inspect
// data is fetched. They're just lazily reported.
async fn record_lazy_values(
    key: String,
    mut stream: fpuppet::LazyInspectPuppetRequestStream,
) -> Result<(), Error> {
    let mut properties = vec![];
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fpuppet::LazyInspectPuppetRequest::RecordString { key, value, responder } => {
                properties.push(Property::String(key, value));
                responder.send().expect("response succeeds")
            }
            fpuppet::LazyInspectPuppetRequest::RecordInt { key, value, responder } => {
                properties.push(Property::Int(key, value));
                responder.send().expect("response succeeds")
            }
            fpuppet::LazyInspectPuppetRequest::Commit { options, responder } => {
                component::inspector().root().record_lazy_values(key, move || {
                    let properties = properties.clone();
                    async move {
                        if options.hang.unwrap_or_default() {
                            Timer::new(zx::MonotonicDuration::from_minutes(60)).await;
                        }
                        let inspector = Inspector::default();
                        let node = inspector.root();
                        for property in properties.iter() {
                            match property {
                                Property::String(k, v) => node.record_string(k, v),
                                Property::Int(k, v) => node.record_int(k, *v),
                                _ => unimplemented!(),
                            }
                        }
                        Ok(inspector)
                    }
                    .boxed()
                });
                responder.send().expect("response succeeds");
                return Ok(()); // drop the connection.
            }
            fpuppet::LazyInspectPuppetRequest::_UnknownMethod { .. } => unreachable!(),
            _ => unimplemented!(),
        };
    }

    Ok(())
}
