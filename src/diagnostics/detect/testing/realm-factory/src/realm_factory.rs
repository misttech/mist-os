// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::realm_events::*;
use crate::realm_options::*;

use {
    anyhow::*,
    component_events::{
        events::{EventStream, Stopped},
        matcher::EventMatcher,
    },
    fake_archive_accessor::FakeArchiveAccessor,
    fidl::endpoints::{create_endpoints, ClientEnd},
    fidl_fuchsia_diagnostics as diagnostics, fidl_fuchsia_diagnostics_test as ftest,
    fidl_fuchsia_feedback as fcrash,
    fidl_fuchsia_io::R_STAR_DIR,
    fidl_server::*,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{
        Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
    },
    fuchsia_zircon as zx,
    futures::lock::Mutex,
    futures::StreamExt,
    std::sync::Arc,
    tracing::{error, info},
};

// Errors returned from this module.
#[derive(Debug, thiserror::Error)]
enum InternalError {
    #[error("Failed to read vmo")]
    ReadVmo(#[source] zx::Status),
}

const DETECT_URL: &str = "#meta/triage-detect.cm";
const ERR_REALM_ALREADY_CREATED: &str = "the realm has already been created";
const ERR_EVENTS_UNAVAILABLE: &str = "you must create the realm before listening for events";

pub(crate) struct RealmFactory {
    realm_options: Option<RealmOptions>,
    events_client: Option<ClientEnd<ftest::TriageDetectEventsMarker>>,
}

impl RealmFactory {
    pub fn new() -> Self {
        Self { events_client: None, realm_options: Some(RealmOptions::new()) }
    }

    pub fn set_realm_options(&mut self, options: ftest::RealmOptions) -> Result<(), Error> {
        match self.realm_options {
            None => bail!(ERR_REALM_ALREADY_CREATED),
            Some(_) => {
                self.realm_options.replace(RealmOptions::from(options));
            }
        }
        Ok(())
    }

    pub fn get_events_client(
        &mut self,
    ) -> Result<ClientEnd<ftest::TriageDetectEventsMarker>, Error> {
        match self.events_client.take() {
            None => bail!(ERR_EVENTS_UNAVAILABLE),
            Some(listener) => Ok(listener),
        }
    }

    pub async fn create_realm(&mut self) -> Result<RealmInstance, Error> {
        if self.realm_options.is_none() {
            bail!(ERR_REALM_ALREADY_CREATED);
        }

        let opts = self.realm_options.take().unwrap();

        let (client, server) = create_endpoints::<ftest::TriageDetectEventsMarker>();
        let event_sender = TriageDetectEventSender::from(server);
        self.events_client.replace(client);

        let builder = RealmBuilder::new().await?;
        let detect = builder.add_child("detect", DETECT_URL, ChildOptions::new().eager()).await?;

        // This is necessary because add_local_child takes a Fn rather than FnOnce, so we cannot move
        // opts out of the local child closure below. However, it is a fatal error to call the closure
        // multiple times anyway - the mock component implementation modifies the realm options.
        let mock_fn = {
            let event_sender = event_sender.clone();
            let closure =
                |handles| async move { serve_mocks(opts, event_sender.clone(), handles).await };
            Arc::new(Mutex::new(Some(closure)))
        };

        let mocks = builder
            .add_local_child(
                "mocks",
                move |handles| {
                    let mock_fn = mock_fn.clone();
                    Box::pin(async move {
                        let mock_fn = mock_fn.lock().await.take().unwrap();
                        mock_fn(handles).await
                    })
                },
                ChildOptions::new(),
            )
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.feedback.CrashReporter"))
                    .capability(Capability::protocol_by_name(
                        "fuchsia.feedback.CrashReportingProductRegister",
                    ))
                    .capability(Capability::protocol_by_name(
                        "fuchsia.diagnostics.FeedbackArchiveAccessor",
                    ))
                    .capability(
                        Capability::directory("config-data")
                            .path("/config/data")
                            .rights(R_STAR_DIR),
                    )
                    .from(&mocks)
                    .to(&detect),
            )
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                    .from(Ref::parent())
                    .to(&detect)
                    .to(&mocks),
            )
            .await?;

        let realm = builder.build().await?;

        // Notify the test when triage-detect terminates.
        let triage_detect_moniker = format!(".*{}.*detect$", realm.root.child_name());
        fasync::Task::spawn(async move {
            let mut event_stream = EventStream::open().await.unwrap();
            EventMatcher::ok()
                .stop(None)
                .moniker_regex(triage_detect_moniker)
                .wait::<Stopped>(&mut event_stream)
                .await
                .unwrap();
            event_sender.send_on_bail();
        })
        .detach();

        Ok(realm)
    }
}

async fn serve_mocks(
    opts: RealmOptions,
    event_sender: TriageDetectEventSender,
    handles: LocalComponentHandles,
) -> Result<(), Error> {
    let mut inspect_data = vec![];

    // Read the inspect data to pass to the archive accessor.
    for contents_vmo in opts.inspect_data.into_iter() {
        let contents = read_string_from_vmo(&contents_vmo).unwrap();
        inspect_data.push(contents);
    }

    // Create the /config/data directory.
    let mut fs = ServiceFs::new();
    let mut dir_config = fs.dir("config");
    let mut dir_config_data = dir_config.dir("data");
    for config_file in opts.config_files.into_iter() {
        dir_config_data.add_vmo_file_at(config_file.0, config_file.1);
    }

    // Serve crash reporter, crash reporting product register, and archive accessor.
    fs.dir("svc")
        .add_fidl_service(|stream: fcrash::CrashReporterRequestStream| {
            serve_async_detached(stream, MockComponent { sender: Some(event_sender.clone()) });
        })
        .add_fidl_service(|stream: fcrash::CrashReportingProductRegisterRequestStream| {
            serve_async_detached(stream, MockComponent { sender: Some(event_sender.clone()) });
        })
        .add_fidl_service_at(
            "fuchsia.diagnostics.FeedbackArchiveAccessor",
            |stream: diagnostics::ArchiveAccessorRequestStream| {
                FakeArchiveAccessor::new(&inspect_data, Some(Box::new(event_sender.clone())))
                    .serve_async(stream);
            },
        );

    fs.serve_connection(handles.outgoing_dir).unwrap();
    fs.collect::<()>().await;
    Ok(())
}

#[derive(Clone)]
pub struct MockComponent {
    sender: Option<TriageDetectEventSender>,
}

#[async_trait::async_trait]
impl AsyncRequestHandler<fcrash::CrashReporterMarker> for MockComponent {
    async fn handle_request(&self, request: fcrash::CrashReporterRequest) -> Result<(), Error> {
        info!("received {:?}", request);
        match request {
            fcrash::CrashReporterRequest::FileReport { report, responder } => {
                let fcrash::CrashReport { program_name, crash_signature, .. } = report;
                let program_name = program_name.unwrap_or("".to_string());
                let crash_signature = crash_signature.unwrap_or("".to_string());
                if let Some(sender) = &self.sender {
                    sender.send_crash_report(&crash_signature, &program_name);
                }
                responder
                    .send(
                        Ok(&fcrash::FileReportResults { ..Default::default() })
                            .map_err(|_| fcrash::FilingError::Unknown),
                    )
                    .context("failed to send response to client")?;
            }

            fidl_fuchsia_feedback::CrashReporterRequest::File { report, responder } => {
                let fcrash::CrashReport { program_name, crash_signature, .. } = report;
                let program_name = program_name.unwrap_or("".to_string());
                let crash_signature = crash_signature.unwrap_or("".to_string());
                if let Some(sender) = &self.sender {
                    sender.send_crash_report(&crash_signature, &program_name);
                }
                responder
                    .send(Ok(()).map_err(|_| 0))
                    .context("failed to send response to client")?;
            }
        };

        Ok(())
    }
}

#[async_trait::async_trait]
impl AsyncRequestHandler<fcrash::CrashReportingProductRegisterMarker> for MockComponent {
    async fn handle_request(
        &self,
        request: fcrash::CrashReportingProductRegisterRequest,
    ) -> Result<(), Error> {
        info!("received {:?}", request);
        match request {
            fcrash::CrashReportingProductRegisterRequest::Upsert { .. } => {
                error!("We shouldn't be calling upsert")
            }
            fcrash::CrashReportingProductRegisterRequest::UpsertWithAck { responder, .. } => {
                responder.send()?;
            }
        };

        Ok(())
    }
}

fn read_string_from_vmo(vmo: &zx::Vmo) -> Result<String, Error> {
    let size: usize = vmo.get_content_size()?.try_into()?;
    let mut buf = vec![0; size];
    vmo.read(&mut buf, 0).map_err(InternalError::ReadVmo)?;
    let the_string = std::str::from_utf8(&buf).unwrap().to_string();
    Ok(the_string)
}
