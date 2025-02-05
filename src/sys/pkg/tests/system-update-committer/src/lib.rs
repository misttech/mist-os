// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(clippy::let_unit_value)]

use anyhow::anyhow;
use assert_matches::assert_matches;
use diagnostics_assertions::{assert_data_tree, AnyProperty};
use diagnostics_hierarchy::DiagnosticsHierarchy;
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl_fuchsia_hardware_power_statecontrol::{RebootOptions, RebootReason2};
use fidl_fuchsia_paver::{self as fpaver, Configuration, ConfigurationStatus};
use fidl_fuchsia_update::{CommitStatusProviderMarker, CommitStatusProviderProxy};
use fuchsia_async::{self as fasync, OnSignals, TimeoutExt};
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_sync::Mutex;
use futures::channel::oneshot;
use futures::prelude::*;
use mock_paver::{hooks as mphooks, MockPaverService, MockPaverServiceBuilder, PaverEvent};
use mock_reboot::MockRebootService;
use mock_verifier::MockVerifierService;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use test_case::test_case;
use zx::{self as zx, AsHandleRef};
use {fidl_fuchsia_io as fio, fidl_fuchsia_update_verify as fupdate_verify};

const SYSTEM_UPDATE_COMMITTER_CM: &str = "#meta/system-update-committer.cm";
const HANG_DURATION: Duration = Duration::from_millis(500);

struct TestEnvBuilder {
    config_data: Option<(PathBuf, String)>,
    paver_service_builder: Option<MockPaverServiceBuilder>,
    reboot_service: Option<MockRebootService>,
    blobfs_verifier_service: Option<MockVerifierService>,
    netstack_verifier_service: Option<MockVerifierService>,
    idle_timeout_millis: Option<i64>,
}
impl TestEnvBuilder {
    fn config_data(self, path: impl Into<PathBuf>, data: impl Into<String>) -> Self {
        Self { config_data: Some((path.into(), data.into())), ..self }
    }

    fn paver_service_builder(self, paver_service_builder: MockPaverServiceBuilder) -> Self {
        Self { paver_service_builder: Some(paver_service_builder), ..self }
    }

    fn reboot_service(self, reboot_service: MockRebootService) -> Self {
        Self { reboot_service: Some(reboot_service), ..self }
    }

    fn blobfs_verifier_service(self, blobfs_verifier_service: MockVerifierService) -> Self {
        Self { blobfs_verifier_service: Some(blobfs_verifier_service), ..self }
    }

    fn netstack_verifier_service(self, netstack_verifier_service: MockVerifierService) -> Self {
        Self { netstack_verifier_service: Some(netstack_verifier_service), ..self }
    }

    fn idle_timeout_millis(self, idle_timeout_millis: i64) -> Self {
        assert_eq!(self.idle_timeout_millis, None);
        Self { idle_timeout_millis: Some(idle_timeout_millis), ..self }
    }

    async fn build(self) -> TestEnv {
        // Optionally write config data.
        let config_data = tempfile::tempdir().expect("/tmp to exist");
        if let Some((path, data)) = self.config_data {
            let path = config_data.path().join(path);
            assert!(!path.exists());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }

        let mut fs = ServiceFs::new();
        let config_data_proxy = fuchsia_fs::directory::open_in_namespace(
            config_data.path().to_str().unwrap(),
            fuchsia_fs::PERM_READABLE | fuchsia_fs::PERM_WRITABLE,
        )
        .unwrap();
        fs.dir("config").add_remote("data", config_data_proxy);

        let mut svc = fs.dir("svc");

        // Set up paver service.
        let paver_service_builder =
            self.paver_service_builder.unwrap_or_else(MockPaverServiceBuilder::new);
        let paver_service = Arc::new(paver_service_builder.build());
        {
            let paver_service = Arc::clone(&paver_service);
            svc.add_fidl_service(move |stream| {
                fasync::Task::spawn(
                    Arc::clone(&paver_service).run_paver_service(stream).unwrap_or_else(|e| {
                        panic!("error running paver service: {:#}", anyhow!(e))
                    }),
                )
                .detach()
            });
        }

        // Set up reboot service.
        let reboot_service = Arc::new(self.reboot_service.unwrap_or_else(|| {
            MockRebootService::new(Box::new(|_| panic!("unexpected call to reboot")))
        }));
        {
            let reboot_service = Arc::clone(&reboot_service);
            svc.add_fidl_service(move |stream| {
                fasync::Task::spawn(
                    Arc::clone(&reboot_service).run_reboot_service(stream).unwrap_or_else(|e| {
                        panic!("error running reboot service: {:#}", anyhow!(e))
                    }),
                )
                .detach()
            });
        }

        // Set up verifier services.
        let blobfs_verifier_service = Arc::new(
            self.blobfs_verifier_service.unwrap_or_else(|| MockVerifierService::new(|_| Ok(()))),
        );
        {
            let verifier_service = Arc::clone(&blobfs_verifier_service);
            svc.add_fidl_service(move |stream| {
                fasync::Task::spawn(
                    Arc::clone(&verifier_service).run_blobfs_verifier_service(stream),
                )
                .detach()
            });
        }
        let netstack_verifier_service = Arc::new(
            self.netstack_verifier_service.unwrap_or_else(|| MockVerifierService::new(|_| Ok(()))),
        );
        {
            let verifier_service = Arc::clone(&netstack_verifier_service);
            svc.add_fidl_service(move |stream| {
                fasync::Task::spawn(
                    Arc::clone(&verifier_service).run_netstack_verifier_service(stream),
                )
                .detach()
            });
        }

        let fs_holder = Mutex::new(Some(fs));
        let builder = RealmBuilder::new().await.expect("Failed to create test realm builder");
        let system_update_committer = builder
            .add_child(
                "system_update_committer",
                SYSTEM_UPDATE_COMMITTER_CM,
                ChildOptions::new().eager(),
            )
            .await
            .unwrap();
        let fake_capabilities = builder
            .add_local_child(
                "fake_capabilities",
                move |handles| {
                    let mut rfs = fs_holder
                        .lock()
                        .take()
                        .expect("mock component should only be launched once");
                    async {
                        let _ = &handles;
                        rfs.serve_connection(handles.outgoing_dir).unwrap();
                        let () = rfs.collect().await;
                        Ok(())
                    }
                    .boxed()
                },
                ChildOptions::new(),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fidl_fuchsia_logger::LogSinkMarker>())
                    .from(Ref::parent())
                    .to(&system_update_committer),
            )
            .await
            .unwrap();
        for (config_name, value) in [(
            "fuchsia.system-update-committer.StopOnIdleTimeoutMillis",
            self.idle_timeout_millis.unwrap_or(-1).into(),
        )] {
            builder
                .add_capability(
                    cm_rust::ConfigurationDecl { name: config_name.parse().unwrap(), value }.into(),
                )
                .await
                .unwrap();
        }
        builder
            .add_route(
                Route::new()
                    .capability(Capability::configuration(
                        "fuchsia.system-update-committer.StopOnIdleTimeoutMillis",
                    ))
                    .from(Ref::self_())
                    .to(&system_update_committer),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("config-data")
                            .path("/config/data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .capability(Capability::protocol::<
                        fidl_fuchsia_hardware_power_statecontrol::AdminMarker,
                    >())
                    .capability(Capability::protocol::<fpaver::PaverMarker>())
                    .capability(Capability::protocol::<fupdate_verify::BlobfsVerifierMarker>())
                    .capability(Capability::protocol::<fupdate_verify::NetstackVerifierMarker>())
                    .from(&fake_capabilities)
                    .to(&system_update_committer),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<CommitStatusProviderMarker>())
                    .from(&system_update_committer)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        let realm_instance = builder.build().await.unwrap();
        let commit_status_provider = realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<CommitStatusProviderMarker>()
            .expect("connect to commit status provider");

        TestEnv {
            _config_data: config_data,
            realm_instance,
            commit_status_provider,
            paver_service,
            _reboot_service: reboot_service,
            _blobfs_verifier_service: blobfs_verifier_service,
            _netstack_verifier_service: netstack_verifier_service,
        }
    }
}
struct TestEnv {
    _config_data: TempDir,
    realm_instance: RealmInstance,
    commit_status_provider: CommitStatusProviderProxy,
    paver_service: Arc<MockPaverService>,
    _reboot_service: Arc<MockRebootService>,
    _blobfs_verifier_service: Arc<MockVerifierService>,
    _netstack_verifier_service: Arc<MockVerifierService>,
}

impl TestEnv {
    fn builder() -> TestEnvBuilder {
        TestEnvBuilder {
            config_data: None,
            paver_service_builder: None,
            reboot_service: None,
            blobfs_verifier_service: None,
            netstack_verifier_service: None,
            idle_timeout_millis: None,
        }
    }

    /// Obtains a clone of the initial connection to fuchsia.update/CommitStatusProvider.
    fn commit_status_provider_proxy(&self) -> CommitStatusProviderProxy {
        self.commit_status_provider.clone()
    }

    /// Obtains a new connection to fuchsia.update/CommitStatusProvider.
    fn fresh_commit_status_provider_proxy(&self) -> CommitStatusProviderProxy {
        self.realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<CommitStatusProviderMarker>()
            .expect("connect to commit status provider")
    }

    async fn system_update_committer_inspect_hierarchy(&self) -> DiagnosticsHierarchy {
        let mut data = ArchiveReader::new()
            .add_selector(format!(
                "realm_builder\\:{}/system_update_committer:root",
                self.realm_instance.root.child_name()
            ))
            .snapshot::<Inspect>()
            .await
            .expect("got inspect data");
        assert_eq!(data.len(), 1, "expected 1 match: {data:?}");
        data.pop().expect("one result").payload.expect("payload is not none")
    }

    async fn wait_for_started(&self, event_stream: &mut component_events::events::EventStream) {
        component_events::matcher::EventMatcher::ok()
            .moniker_regex(format!(
                "^realm_builder:{}/system_update_committer$",
                self.realm_instance.root.child_name()
            ))
            .wait::<component_events::events::Started>(event_stream)
            .await
            .unwrap();
    }

    async fn wait_for_clean_stopped(
        &self,
        event_stream: &mut component_events::events::EventStream,
    ) {
        let stopped = component_events::matcher::EventMatcher::ok()
            .moniker_regex(format!(
                "^realm_builder:{}/system_update_committer$",
                self.realm_instance.root.child_name()
            ))
            .wait::<component_events::events::Stopped>(event_stream)
            .await
            .unwrap();
        assert_matches!(
            stopped.result().unwrap(),
            component_events::events::StoppedPayload {
                status: component_events::events::ExitStatus::Clean,
                exit_code: Some(0)
            }
        );
    }
}

/// IsCurrentSystemCommitted should hang until when the Paver responds to
/// QueryConfigurationStatusAndBootAttempts.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn is_current_system_committed_hangs_until_query_configuration_status(
    idle_timeout_millis: i64,
) {
    let (throttle_hook, throttler) = mphooks::throttle();

    let env = TestEnv::builder()
        .idle_timeout_millis(idle_timeout_millis)
        .paver_service_builder(MockPaverServiceBuilder::new().insert_hook(throttle_hook))
        .build()
        .await;

    // No paver events yet, so the commit status FIDL server is still hanging.
    // We use timeouts to tell if the FIDL server hangs. This is obviously not ideal, but
    // there does not seem to be a better way of doing it. We considered using `run_until_stalled`,
    // but that's no good because the system-update-committer is running in a seperate process.
    assert_matches!(
        env.commit_status_provider_proxy()
            .is_current_system_committed()
            .map(Some)
            .on_timeout(HANG_DURATION, || None)
            .await,
        None
    );

    // Even after the first paver response, is_current_system_committed should still hang.
    let () = throttler.emit_next_paver_event(&PaverEvent::QueryCurrentConfiguration);
    assert_matches!(
        env.commit_status_provider_proxy()
            .is_current_system_committed()
            .map(Some)
            .on_timeout(HANG_DURATION, || None)
            .await,
        None
    );

    // After the second paver event, we're finally unblocked.
    let () =
        throttler.emit_next_paver_event(&PaverEvent::QueryConfigurationStatusAndBootAttempts {
            configuration: Configuration::A,
        });
    env.commit_status_provider_proxy().is_current_system_committed().await.unwrap();
}

/// If the current system is pending commit, the commit status FIDL server should hang
/// until the verifiers start (e.g. after we call QueryConfigurationStatusAndBootAttempts). Once the
/// verifications complete, we should observe the `USER_0` signal.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn system_pending_commit(idle_timeout_millis: i64) {
    let (throttle_hook, throttler) = mphooks::throttle();

    let env = TestEnv::builder()
        .idle_timeout_millis(idle_timeout_millis)
        .paver_service_builder(
            MockPaverServiceBuilder::new().insert_hook(throttle_hook).insert_hook(
                mphooks::config_status_and_boot_attempts(|_| {
                    Ok((ConfigurationStatus::Pending, Some(1)))
                }),
            ),
        )
        .build()
        .await;

    // Emit the first 2 paver events to unblock the FIDL server.
    let () = throttler.emit_next_paver_events(&[
        PaverEvent::QueryCurrentConfiguration,
        PaverEvent::QueryConfigurationStatusAndBootAttempts { configuration: Configuration::A },
    ]);
    let event_pair =
        env.commit_status_provider_proxy().is_current_system_committed().await.unwrap();
    assert_eq!(
        event_pair.wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE_PAST),
        Err(zx::Status::TIMED_OUT)
    );

    // Once the remaining paver calls are emitted, the system should commit.
    let () = throttler.emit_next_paver_events(&[
        PaverEvent::SetConfigurationHealthy { configuration: Configuration::A },
        PaverEvent::SetConfigurationUnbootable { configuration: Configuration::B },
        PaverEvent::BootManagerFlush,
    ]);
    assert_eq!(OnSignals::new(&event_pair, zx::Signals::USER_0).await, Ok(zx::Signals::USER_0));

    let () = fasync::Timer::new(std::time::Duration::from_millis(500)).await;

    // Observe boot_attempts shows up in inspect.
    let hierarchy = env.system_update_committer_inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "commit": {
                "boot_attempts": 1u64,
            }
        }
    );
}

/// If the current system is already committed, the EventPair returned should immediately have
/// `USER_0` asserted.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn system_already_committed(idle_timeout_millis: i64) {
    let (throttle_hook, throttler) = mphooks::throttle();

    let env = TestEnv::builder()
        .idle_timeout_millis(idle_timeout_millis)
        .paver_service_builder(MockPaverServiceBuilder::new().insert_hook(throttle_hook))
        .build()
        .await;

    // Emit the first 2 paver events to unblock the FIDL server.
    let () = throttler.emit_next_paver_events(&[
        PaverEvent::QueryCurrentConfiguration,
        PaverEvent::QueryConfigurationStatusAndBootAttempts { configuration: Configuration::A },
    ]);

    // When the commit status FIDL responds, the event pair should immediately observe the signal.
    let event_pair =
        env.commit_status_provider_proxy().is_current_system_committed().await.unwrap();
    assert_eq!(
        event_pair.wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE_PAST),
        Ok(zx::Signals::USER_0)
    );
}

/// There's some complexity with how we handle CommitStatusProvider requests. So, let's do
/// a sanity check to verify our implementation can handle multiple clients.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn multiple_commit_status_provider_requests(idle_timeout_millis: i64) {
    let env = TestEnv::builder().idle_timeout_millis(idle_timeout_millis).build().await;

    let p0 = env.commit_status_provider_proxy().is_current_system_committed().await.unwrap();
    let p1 = env.commit_status_provider_proxy().is_current_system_committed().await.unwrap();

    assert_eq!(
        p0.wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE_PAST),
        Ok(zx::Signals::USER_0)
    );
    assert_eq!(
        p1.wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE_PAST),
        Ok(zx::Signals::USER_0)
    );
}

/// Make sure the inspect data is plumbed through after successful verifications.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn inspect_health_status_ok(idle_timeout_millis: i64) {
    let env = TestEnv::builder()
        .idle_timeout_millis(idle_timeout_millis)
        // Make sure we run health verifications.
        .paver_service_builder(MockPaverServiceBuilder::new().insert_hook(
            mphooks::config_status_and_boot_attempts(|_| {
                Ok((ConfigurationStatus::Pending, Some(1)))
            }),
        ))
        .build()
        .await;

    // Wait for verifications to complete.
    let p = env.commit_status_provider_proxy().is_current_system_committed().await.unwrap();
    assert_eq!(OnSignals::new(&p, zx::Signals::USER_0).await, Ok(zx::Signals::USER_0));

    // Observe verification shows up in inspect.
    let hierarchy = env.system_update_committer_inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: {
            "structured_config": contains {},
            "verification": {
                "ota_verification_duration": {
                    "success": AnyProperty,
                }
            },
            "fuchsia.inspect.Health": {
                "start_timestamp_nanos": AnyProperty,
                "status": "OK"
            },
            "commit": {
                "boot_attempts": 1u64
            }
        }
    );
}

/// Make sure the inspect data correctly report multiple verification failures.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn inspect_multiple_failures(idle_timeout_millis: i64) {
    let env = TestEnv::builder()
        .idle_timeout_millis(idle_timeout_millis)
        // Make sure we run health verifications.
        .paver_service_builder(MockPaverServiceBuilder::new().insert_hook(
            mphooks::config_status_and_boot_attempts(|_| {
                Ok((ConfigurationStatus::Pending, Some(1)))
            }),
        ))
        .blobfs_verifier_service(MockVerifierService::new(|_| {
            Err(fidl_fuchsia_update_verify::VerifyError::Internal)
        }))
        .netstack_verifier_service(MockVerifierService::new(|_| {
            Err(fidl_fuchsia_update_verify::VerifyError::Internal)
        }))
        .build()
        .await;

    // Wait for verifications to complete.
    let p = env.commit_status_provider_proxy().is_current_system_committed().await.unwrap();
    assert_eq!(OnSignals::new(&p, zx::Signals::USER_0).await, Ok(zx::Signals::USER_0));

    // Observe verification shows up in inspect.
    let hierarchy = env.system_update_committer_inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: {
            "structured_config": contains {},
            "verification": {
                "ota_verification_duration": {
                    "failure_blobfs": AnyProperty,
                    "failure_netstack": AnyProperty,
                },
                "ota_verification_failure": {
                    "blobfs_verify": 1u64,
                    "netstack_verify": 1u64,
                }
            },
            "fuchsia.inspect.Health": {
                "start_timestamp_nanos": AnyProperty,
                "status": "OK",
            },
            "commit": {
                "boot_attempts": 1u64
            }
        }
    );
}

/// When the paver fails, the system-update-committer should trigger a reboot regardless of the
/// config. Additionally, the inspect state should reflect the system being unhealthy. We could
/// split this up into several tests, but instead we combine them to reduce redundant lines of code.
/// We could do helper fns, but we decided not to given guidance in
/// https://testing.googleblog.com/2019/12/testing-on-toilet-tests-too-dry-make.html.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn paver_failure_causes_reboot(idle_timeout_millis: i64) {
    let (reboot_sender, reboot_recv) = oneshot::channel();
    let reboot_sender = Arc::new(Mutex::new(Some(reboot_sender)));
    let env = TestEnv::builder()
        .idle_timeout_millis(idle_timeout_millis)
        // Make sure the paver fails.
        .paver_service_builder(MockPaverServiceBuilder::new().insert_hook(mphooks::return_error(
            |e: &PaverEvent| {
                if e == &PaverEvent::QueryCurrentConfiguration {
                    zx::Status::NOT_FOUND
                } else {
                    zx::Status::OK
                }
            },
        )))
        // Make the config say that verification errors should be ignored. This shouldn't have any
        // effect on whether the paver failures cause a reboot.
        .config_data("config.json", json!({"blobfs": "ignore"}).to_string())
        // Handle the reboot requests.
        .reboot_service(MockRebootService::new(Box::new(move |options: RebootOptions| {
            reboot_sender.lock().take().unwrap().send(options).unwrap();
            Ok(())
        })))
        .build()
        .await;

    assert_eq!(
        reboot_recv.await,
        Ok(RebootOptions {
            reasons: Some(vec![RebootReason2::RetrySystemUpdate]),
            ..Default::default()
        })
    );

    let hierarchy = env.system_update_committer_inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: {
            "structured_config": contains {},
            "verification": {},
            "fuchsia.inspect.Health": {
                "message": AnyProperty,
                "start_timestamp_nanos": AnyProperty,
                "status": "UNHEALTHY"
            },
            "commit": {}
        }
    );
}

/// When the blobfs verifications fail and the config says to reboot, we should reboot.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn blobfs_verification_failure_causes_reboot(idle_timeout_millis: i64) {
    let (reboot_sender, reboot_recv) = oneshot::channel();
    let reboot_sender = Arc::new(Mutex::new(Some(reboot_sender)));
    let env = TestEnv::builder()
        .idle_timeout_millis(idle_timeout_millis)
        // Make sure we run health verifications.
        .paver_service_builder(MockPaverServiceBuilder::new().insert_hook(
            mphooks::config_status_and_boot_attempts(|_| {
                Ok((ConfigurationStatus::Pending, Some(1)))
            }),
        ))
        // Make the blobfs health verifications fail.
        .blobfs_verifier_service(MockVerifierService::new(|_| {
            Err(fidl_fuchsia_update_verify::VerifyError::Internal)
        }))
        // Make us reboot on failure.
        .config_data("config.json", json!({"blobfs": "reboot_on_failure"}).to_string())
        // Handle the reboot requests.
        .reboot_service(MockRebootService::new(Box::new(move |options: RebootOptions| {
            reboot_sender.lock().take().unwrap().send(options).unwrap();
            Ok(())
        })))
        .build()
        .await;

    // We should observe a reboot.
    assert_eq!(
        reboot_recv.await,
        Ok(RebootOptions {
            reasons: Some(vec![RebootReason2::RetrySystemUpdate]),
            ..Default::default()
        })
    );

    // Observe failed verification shows up in inspect.
    let hierarchy = env.system_update_committer_inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: {
            "structured_config": contains {},
            "verification": {
                "ota_verification_duration": {
                    "failure_blobfs": AnyProperty,
                },
                "ota_verification_failure": {
                    "blobfs_verify": 1u64,
                }
            },
            "fuchsia.inspect.Health": {
                "message": AnyProperty,
                "start_timestamp_nanos": AnyProperty,
                "status": "UNHEALTHY"
            },
            "commit": {}
        }
    );
}

/// When the netstack verifications fail and the config says to reboot, we should reboot.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn netstack_verification_failure_causes_reboot(idle_timeout_millis: i64) {
    let (reboot_sender, reboot_recv) = oneshot::channel();
    let reboot_sender = Arc::new(Mutex::new(Some(reboot_sender)));
    let env = TestEnv::builder()
        .idle_timeout_millis(idle_timeout_millis)
        // Make sure we run health verifications.
        .paver_service_builder(MockPaverServiceBuilder::new().insert_hook(
            mphooks::config_status_and_boot_attempts(|_| {
                Ok((ConfigurationStatus::Pending, Some(1)))
            }),
        ))
        // Make the netstack health verifications fail.
        .netstack_verifier_service(MockVerifierService::new(|_| {
            Err(fidl_fuchsia_update_verify::VerifyError::Internal)
        }))
        // Make us reboot on failure.
        .config_data("config.json", json!({"netstack": "reboot_on_failure"}).to_string())
        // Handle the reboot requests.
        .reboot_service(MockRebootService::new(Box::new(move |options: RebootOptions| {
            reboot_sender.lock().take().unwrap().send(options).unwrap();
            Ok(())
        })))
        .build()
        .await;

    // We should observe a reboot.
    assert_eq!(
        reboot_recv.await,
        Ok(RebootOptions {
            reasons: Some(vec![RebootReason2::RetrySystemUpdate]),
            ..Default::default()
        })
    );

    // Observe failed verification shows up in inspect.
    let hierarchy = env.system_update_committer_inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: {
            "structured_config": contains {},
            "verification": {
                "ota_verification_duration": {
                    "failure_netstack": AnyProperty,
                },
                "ota_verification_failure": {
                    "netstack_verify": 1u64,
                }
            },
            "fuchsia.inspect.Health": {
                "message": AnyProperty,
                "start_timestamp_nanos": AnyProperty,
                "status": "UNHEALTHY"
            },
            "commit": {}
        }
    );
}

/// When the verifications fail and the config says NOT to reboot, we should NOT reboot.
#[test_case(-1i64; "never idle")]
#[test_case(0i64; "rapid idle")]
#[fasync::run_singlethreaded(test)]
async fn verification_failure_does_not_cause_reboot(idle_timeout_millis: i64) {
    let (reboot_sender, mut reboot_recv) = oneshot::channel();
    let reboot_sender = Arc::new(Mutex::new(Some(reboot_sender)));
    let env = TestEnv::builder()
        .idle_timeout_millis(idle_timeout_millis)
        // Make sure we run health verifications.
        .paver_service_builder(MockPaverServiceBuilder::new().insert_hook(
            mphooks::config_status_and_boot_attempts(|_| {
                Ok((ConfigurationStatus::Pending, Some(1)))
            }),
        ))
        // Make the health verifications fail.
        .blobfs_verifier_service(MockVerifierService::new(|_| {
            Err(fidl_fuchsia_update_verify::VerifyError::Internal)
        }))
        // Make us IGNORE the verification failure.
        .config_data("config.json", json!({"blobfs": "ignore"}).to_string())
        // Handle the reboot requests.
        .reboot_service(MockRebootService::new(Box::new(move |options: RebootOptions| {
            reboot_sender.lock().take().unwrap().send(options).unwrap();
            Ok(())
        })))
        .build()
        .await;

    // The commit should happen because the failure was ignored.
    let p = env.commit_status_provider_proxy().is_current_system_committed().await.unwrap();
    assert_eq!(OnSignals::new(&p, zx::Signals::USER_0).await, Ok(zx::Signals::USER_0));

    // We should NOT observe a reboot.
    assert_eq!(reboot_recv.try_recv(), Ok(None));

    // Observe failed verification shows up in inspect.
    let hierarchy = env.system_update_committer_inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: {
            "structured_config": contains {},
            "verification": {
                "ota_verification_duration": {
                    "failure_blobfs": AnyProperty,
                },
                "ota_verification_failure": {
                    "blobfs_verify": 1u64,
                }
            },
            "fuchsia.inspect.Health": {
                "start_timestamp_nanos": AnyProperty,
                "status": "OK"
            },
            "commit": {
                "boot_attempts": 1u64
            }
        }
    );
}

// If configured, when the system-update-checker is idle (when there has not been any activity on
// its out dir or any outstanding fidl connections for a period of time), the system-update-checker
// escrows its state with the CM and stops itself. Later, when there is activity again, CM restarts
// the system-update-checker which should then retrieve the escrowed state and handle the activity
// until it is time to idle-stop again.
// This tests that the system-update-checker stops when idle and correctly resumes from its escrowed
// state, which includes verifying that:
// 1. the paver is only interacted with once, on the first run
// 2. activity on connections that existed when the component stopped itself will restart the
//    component and be handled correctly
// 3. activity on the out dir while the component is stopped will restart the component and be
//    handled correctly
// 4. inspect data is escrowed
// 5. the internal end of the event pair is escrowed
#[fasync::run_singlethreaded(test)]
async fn stop_on_idle_resume_on_use() {
    let mut event_stream = component_events::events::EventStream::open().await.unwrap();
    let (throttle_hook, throttler) = mphooks::throttle();
    let env = TestEnv::builder()
        .idle_timeout_millis(0)
        .paver_service_builder(
            MockPaverServiceBuilder::new().insert_hook(throttle_hook).insert_hook(
                mphooks::config_status_and_boot_attempts(|_| {
                    Ok((ConfigurationStatus::Pending, Some(1)))
                }),
            ),
        )
        .build()
        .await;

    // The component has eager startup.
    env.wait_for_started(&mut event_stream).await;

    // Emit the first 2 paver events to unblock the FIDL server.
    let () = throttler.emit_next_paver_events(&[
        PaverEvent::QueryCurrentConfiguration,
        PaverEvent::QueryConfigurationStatusAndBootAttempts { configuration: Configuration::A },
    ]);

    // Obtain a connection to the component. We need to make a call on the connection to force the
    // CM to give the server end to the component because of the use of `delivery: "on_readable"` in
    // the manifest.
    let proxy0 = env.commit_status_provider_proxy();
    let event_pair0 = proxy0.is_current_system_committed().await.unwrap();

    // The component should not stop on idle before committing the system.
    let () = fasync::Timer::new(std::time::Duration::from_secs(3)).await;

    // Once the remaining paver calls are emitted, the system should commit.
    let () = throttler.emit_next_paver_events(&[
        PaverEvent::SetConfigurationHealthy { configuration: Configuration::A },
        PaverEvent::SetConfigurationUnbootable { configuration: Configuration::B },
        PaverEvent::BootManagerFlush,
    ]);
    assert_eq!(OnSignals::new(&event_pair0, zx::Signals::USER_0).await, Ok(zx::Signals::USER_0));

    // With the system committed, the component should stop even though we have a connection to it.
    env.wait_for_clean_stopped(&mut event_stream).await;

    // Inspect data should still be visible even with the component stopped.
    let hierarchy = env.system_update_committer_inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "commit": {
                "boot_attempts": 1u64,
            }
        }
    );

    // Interacting with the existing connection should start the component.
    let event_pair1 = proxy0.is_current_system_committed().await.unwrap();
    env.wait_for_started(&mut event_stream).await;

    // The signal should still be asserted.
    assert_eq!(
        event_pair1.wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE_PAST),
        Ok(zx::Signals::USER_0)
    );

    // Component should stop again.
    env.wait_for_clean_stopped(&mut event_stream).await;

    // The component should re-escrow the frozen inspect data it retrieved when re-starting, so
    // inspect should still be visible.
    let hierarchy = env.system_update_committer_inspect_hierarchy().await;
    assert_data_tree!(
        hierarchy,
        root: contains {
            "commit": {
                "boot_attempts": 1u64,
            }
        }
    );

    // Creating a new connection to the component through its out dir should start the component.
    let proxy1 = env.fresh_commit_status_provider_proxy();
    let event_pair2 = proxy1.is_current_system_committed().await.unwrap();
    env.wait_for_started(&mut event_stream).await;
    env.wait_for_clean_stopped(&mut event_stream).await;

    // The signal should still be asserted.
    assert_eq!(
        event_pair2.wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE_PAST),
        Ok(zx::Signals::USER_0)
    );

    // The server's end of the event pair should still be alive.
    assert_eq!(
        event_pair0
            .wait_handle(zx::Signals::EVENTPAIR_PEER_CLOSED, zx::MonotonicInstant::INFINITE_PAST),
        Err(zx::Status::TIMED_OUT)
    );

    // Even though the component started multiple times, it should only have attempted to commit the
    // system once.
    use mock_paver::PaverEvent::*;
    assert_eq!(
        env.paver_service.take_events(),
        vec![
            QueryCurrentConfiguration,
            QueryConfigurationStatusAndBootAttempts { configuration: Configuration::A },
            SetConfigurationHealthy { configuration: Configuration::A },
            SetConfigurationUnbootable { configuration: Configuration::B },
            BootManagerFlush
        ]
    );
}
