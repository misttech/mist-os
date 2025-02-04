// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod rollback;

use std::pin::Pin;

use cobalt_client::traits::AsEventCode as _;
use fuchsia_async::Task;
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use fuchsia_inspect::Property as _;
use futures::channel::mpsc;
use futures::{Stream, StreamExt as _};
use log::{error, info, warn};
use networking_metrics_registry::networking_metrics_registry as metrics_registry;
use {
    fidl_fuchsia_metrics as fmetrics, fidl_fuchsia_net_stackmigrationdeprecated as fnet_migration,
    fidl_fuchsia_power_internal as fpower,
};

const DEFAULT_NETSTACK: NetstackVersion = NetstackVersion::Netstack2;

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, Eq, PartialEq)]
enum NetstackVersion {
    Netstack2,
    Netstack3,
}

impl NetstackVersion {
    fn inspect_uint_value(&self) -> u64 {
        match self {
            Self::Netstack2 => 2,
            Self::Netstack3 => 3,
        }
    }

    fn optional_inspect_uint_value(o: &Option<Self>) -> u64 {
        o.as_ref().map(Self::inspect_uint_value).unwrap_or(0)
    }
}

impl From<fnet_migration::NetstackVersion> for NetstackVersion {
    fn from(value: fnet_migration::NetstackVersion) -> Self {
        match value {
            fnet_migration::NetstackVersion::Netstack2 => NetstackVersion::Netstack2,
            fnet_migration::NetstackVersion::Netstack3 => NetstackVersion::Netstack3,
        }
    }
}

impl From<NetstackVersion> for fnet_migration::NetstackVersion {
    fn from(value: NetstackVersion) -> Self {
        match value {
            NetstackVersion::Netstack2 => fnet_migration::NetstackVersion::Netstack2,
            NetstackVersion::Netstack3 => fnet_migration::NetstackVersion::Netstack3,
        }
    }
}

impl From<NetstackVersion> for Box<fnet_migration::VersionSetting> {
    fn from(value: NetstackVersion) -> Self {
        Box::new(fnet_migration::VersionSetting { version: value.into() })
    }
}

#[derive(Default, Debug, serde::Deserialize, serde::Serialize)]
#[cfg_attr(test, derive(Eq, PartialEq))]
struct Persisted {
    automated: Option<NetstackVersion>,
    user: Option<NetstackVersion>,
    rollback: Option<rollback::Persisted>,
}

impl Persisted {
    fn load<R: std::io::Read>(r: R) -> Self {
        serde_json::from_reader(std::io::BufReader::new(r)).unwrap_or_else(|e| {
            error!("error loading persisted config {e:?}, using defaults");
            Persisted::default()
        })
    }

    fn save<W: std::io::Write>(&self, w: W) {
        serde_json::to_writer(w, self).unwrap_or_else(|e: serde_json::Error| {
            error!("error persisting configuration {self:?}: {e:?}")
        })
    }

    // Determine the desired NetstackVersion based on the persisted values
    fn desired_netstack_version(&self) -> NetstackVersion {
        match self {
            Persisted { user: Some(user), automated: _, rollback: _ } => *user,
            Persisted {
                user: None,
                rollback: Some(rollback::Persisted::HealthcheckFailures(failures)),
                automated: _,
            } if *failures >= rollback::MAX_FAILED_HEALTHCHECKS => NetstackVersion::Netstack2,
            Persisted { user: None, automated: Some(automated), rollback: _ } => *automated,
            // Use the default version if nothing is set.
            Persisted { user: None, automated: None, rollback: _ } => DEFAULT_NETSTACK,
        }
    }
}

enum ServiceRequest {
    Control(fnet_migration::ControlRequest),
    State(fnet_migration::StateRequest),
}

struct Migration<P, CR> {
    current_boot: NetstackVersion,
    persisted: Persisted,
    persistence: P,
    collaborative_reboot: CollaborativeReboot<CR>,
}

trait PersistenceProvider {
    type Writer: std::io::Write;
    type Reader: std::io::Read;

    fn open_writer(&mut self) -> std::io::Result<Self::Writer>;
    fn open_reader(&self) -> std::io::Result<Self::Reader>;
}

struct DataPersistenceProvider {}

const PERSISTED_FILE_PATH: &'static str = "/data/config.json";

impl PersistenceProvider for DataPersistenceProvider {
    type Writer = std::fs::File;
    type Reader = std::fs::File;

    fn open_writer(&mut self) -> std::io::Result<Self::Writer> {
        std::fs::File::create(PERSISTED_FILE_PATH)
    }

    fn open_reader(&self) -> std::io::Result<Self::Reader> {
        std::fs::File::open(PERSISTED_FILE_PATH)
    }
}

struct CollaborativeReboot<CR> {
    scheduler: CR,
    /// `Some(<cancellation_token>)` if there's an outstanding collaborative
    /// reboot scheduled.
    scheduled_req: Option<zx::EventPair>,
}

impl<CR: CollaborativeRebootScheduler> CollaborativeReboot<CR> {
    /// Schedules a collaborative reboot.
    ///
    /// No-Op if there's already a reboot scheduled.
    async fn schedule(&mut self) {
        let Self { scheduler, scheduled_req } = self;
        if scheduled_req.is_some() {
            // We already have an outstanding request.
            return;
        }

        info!("Scheduling collaborative reboot");
        let (mine, theirs) = zx::EventPair::create();
        *scheduled_req = Some(mine);
        scheduler
            .schedule(fpower::CollaborativeRebootReason::NetstackMigration, Some(theirs))
            .await;
    }

    /// Cancels the currently scheduled collaborative Reboot.
    ///
    /// No-Op if there's none scheduled.
    fn cancel(&mut self) {
        if let Some(cancel) = self.scheduled_req.take() {
            info!("Canceling collaborative reboot request. It's no longer necessary.");
            // Dropping the eventpair cancels the request.
            std::mem::drop(cancel);
        }
    }
}

/// An abstraction over the `fpower::CollaborativeRebootScheduler` FIDL API.
trait CollaborativeRebootScheduler {
    async fn schedule(
        &mut self,
        reason: fpower::CollaborativeRebootReason,
        cancel: Option<zx::EventPair>,
    );
}

/// An implementation of `CollaborativeRebootScheduler` that connects to the
/// API over FIDL.
struct Scheduler {}

impl CollaborativeRebootScheduler for Scheduler {
    async fn schedule(
        &mut self,
        reason: fpower::CollaborativeRebootReason,
        cancel: Option<zx::EventPair>,
    ) {
        let proxy = match fuchsia_component::client::connect_to_protocol::<
            fpower::CollaborativeRebootSchedulerMarker,
        >() {
            Ok(proxy) => proxy,
            Err(e) => {
                error!("Failed to connect to collaborative reboot scheduler: {e:?}");
                return;
            }
        };
        match proxy.schedule_reboot(reason, cancel).await {
            Ok(()) => {}
            Err(e) => error!("Failed to schedule collaborative reboot: {e:?}"),
        }
    }
}

impl<P: PersistenceProvider, CR: CollaborativeRebootScheduler> Migration<P, CR> {
    fn new(persistence: P, cr_scheduler: CR) -> Self {
        let persisted = persistence.open_reader().map(Persisted::load).unwrap_or_else(|e| {
            warn!("could not open persistence reader: {e:?}. using defaults");
            Persisted::default()
        });
        let current_boot = persisted.desired_netstack_version();

        Self {
            current_boot,
            persisted,
            persistence,
            collaborative_reboot: CollaborativeReboot {
                scheduler: cr_scheduler,
                scheduled_req: None,
            },
        }
    }

    fn persist(&mut self) {
        let Self { current_boot: _, persisted, persistence, collaborative_reboot: _ } = self;
        let w = match persistence.open_writer() {
            Ok(w) => w,
            Err(e) => {
                error!("failed to open writer to persist settings: {e:?}");
                return;
            }
        };
        persisted.save(w);
    }

    fn map_version_setting(
        version: Option<Box<fnet_migration::VersionSetting>>,
    ) -> Option<NetstackVersion> {
        version.map(|v| {
            let fnet_migration::VersionSetting { version } = &*v;
            (*version).into()
        })
    }

    async fn update_collaborative_reboot(&mut self) {
        let Self { current_boot, persisted, persistence: _, collaborative_reboot } = self;
        if persisted.desired_netstack_version() != *current_boot {
            // When the current boot differs from our desired version, schedule
            // a reboot (if there's not already one).
            collaborative_reboot.schedule().await
        } else {
            // When the current_boot matches our desired version, we no longer
            // need reboot. Cancel the outstanding request (if any)
            collaborative_reboot.cancel()
        }
    }

    async fn update_rollback_state(&mut self, new_state: rollback::Persisted) {
        if self.persisted.rollback != Some(new_state) {
            self.persisted.rollback = Some(new_state);
            self.update_collaborative_reboot().await;
            self.persist();
        }
    }

    async fn handle_control_request(
        &mut self,
        req: fnet_migration::ControlRequest,
    ) -> Result<(), fidl::Error> {
        match req {
            fnet_migration::ControlRequest::SetAutomatedNetstackVersion { version, responder } => {
                let version = Self::map_version_setting(version);
                let Self {
                    current_boot: _,
                    persisted: Persisted { automated, user: _, rollback: _ },
                    persistence: _,
                    collaborative_reboot: _,
                } = self;
                if version != *automated {
                    info!("automated netstack version switched to {version:?}");
                    *automated = version;
                    self.persist();
                    self.update_collaborative_reboot().await;
                }
                responder.send()
            }
            fnet_migration::ControlRequest::SetUserNetstackVersion { version, responder } => {
                let version = Self::map_version_setting(version);
                let Self {
                    current_boot: _,
                    persisted: Persisted { automated: _, user, rollback: _ },
                    persistence: _,
                    collaborative_reboot: _,
                } = self;
                if version != *user {
                    info!("user netstack version switched to {version:?}");
                    *user = version;
                    self.persist();
                    self.update_collaborative_reboot().await;
                }
                responder.send()
            }
        }
    }

    fn handle_state_request(&self, req: fnet_migration::StateRequest) -> Result<(), fidl::Error> {
        let Migration {
            current_boot,
            persisted: Persisted { user, automated, rollback: _ },
            persistence: _,
            collaborative_reboot: _,
        } = self;
        match req {
            fnet_migration::StateRequest::GetNetstackVersion { responder } => {
                responder.send(&fnet_migration::InEffectVersion {
                    current_boot: (*current_boot).into(),
                    user: (*user).map(Into::into),
                    automated: (*automated).map(Into::into),
                })
            }
        }
    }

    async fn handle_request(&mut self, req: ServiceRequest) -> Result<(), fidl::Error> {
        match req {
            ServiceRequest::Control(r) => self.handle_control_request(r).await,
            ServiceRequest::State(r) => self.handle_state_request(r),
        }
    }
}

struct InspectNodes {
    automated_setting: fuchsia_inspect::UintProperty,
    user_setting: fuchsia_inspect::UintProperty,
}

impl InspectNodes {
    fn new<P, CR>(inspector: &fuchsia_inspect::Inspector, m: &Migration<P, CR>) -> Self {
        let root = inspector.root();
        // TODO(https://fxbug.dev/388051523): Add inspect for rollback.
        let Migration {
            current_boot, persisted: Persisted { automated, user, rollback: _ }, ..
        } = m;
        let automated_setting = root.create_uint(
            "automated_setting",
            NetstackVersion::optional_inspect_uint_value(automated),
        );
        let user_setting =
            root.create_uint("user_setting", NetstackVersion::optional_inspect_uint_value(user));

        // The current boot version is immutable, record it once instead of
        // keeping track of a property node.
        root.record_uint("current_boot", current_boot.inspect_uint_value());
        Self { automated_setting, user_setting }
    }

    fn update<P, CR>(&self, m: &Migration<P, CR>) {
        let Migration { persisted: Persisted { automated, user, rollback: _ }, .. } = m;
        let Self { automated_setting, user_setting } = self;
        automated_setting.set(NetstackVersion::optional_inspect_uint_value(automated));
        user_setting.set(NetstackVersion::optional_inspect_uint_value(user));
    }
}

/// Wraps communication with metrics (cobalt) server.
struct MetricsLogger {
    logger: Option<fmetrics::MetricEventLoggerProxy>,
}

impl MetricsLogger {
    async fn new() -> Self {
        let (logger, server_end) =
            fidl::endpoints::create_proxy::<fmetrics::MetricEventLoggerMarker>();

        let factory = match fuchsia_component::client::connect_to_protocol::<
            fmetrics::MetricEventLoggerFactoryMarker,
        >() {
            Ok(f) => f,
            Err(e) => {
                warn!("can't connect to logger factory {e:?}");
                return Self { logger: None };
            }
        };

        match factory
            .create_metric_event_logger(
                &fmetrics::ProjectSpec {
                    customer_id: Some(metrics_registry::CUSTOMER_ID),
                    project_id: Some(metrics_registry::PROJECT_ID),
                    ..Default::default()
                },
                server_end,
            )
            .await
        {
            Ok(Ok(())) => Self { logger: Some(logger) },
            Ok(Err(e)) => {
                warn!("can't create event logger {e:?}");
                Self { logger: None }
            }
            Err(e) => {
                warn!("error connecting to metric event logger {e:?}");
                Self { logger: None }
            }
        }
    }

    /// Logs metrics from `migration` to the metrics server.
    async fn log_metrics<P, CR>(&self, migration: &Migration<P, CR>) {
        let logger = if let Some(logger) = self.logger.as_ref() {
            logger
        } else {
            // Silently don't log metrics if we didn't manage to create a
            // logger, warnings are emitted upon creation.
            return;
        };

        let current_boot = match migration.current_boot {
            NetstackVersion::Netstack2 => {
                metrics_registry::StackMigrationCurrentBootMetricDimensionNetstackVersion::Netstack2
            }
            NetstackVersion::Netstack3 => {
                metrics_registry::StackMigrationCurrentBootMetricDimensionNetstackVersion::Netstack3
            }
        }
        .as_event_code();
        let user = match migration.persisted.user {
            None => metrics_registry::StackMigrationUserSettingMetricDimensionNetstackVersion::NoSelection,
            Some(NetstackVersion::Netstack2) => metrics_registry::StackMigrationUserSettingMetricDimensionNetstackVersion::Netstack2,
            Some(NetstackVersion::Netstack3) => metrics_registry::StackMigrationUserSettingMetricDimensionNetstackVersion::Netstack3,
        }
        .as_event_code();
        let automated = match migration.persisted.automated {
            None => metrics_registry::StackMigrationAutomatedSettingMetricDimensionNetstackVersion::NoSelection,
            Some(NetstackVersion::Netstack2) => metrics_registry::StackMigrationAutomatedSettingMetricDimensionNetstackVersion::Netstack2,
            Some(NetstackVersion::Netstack3) => metrics_registry::StackMigrationAutomatedSettingMetricDimensionNetstackVersion::Netstack3,
        }
        .as_event_code();
        for (metric_id, event_code) in [
            (metrics_registry::STACK_MIGRATION_CURRENT_BOOT_METRIC_ID, current_boot),
            (metrics_registry::STACK_MIGRATION_USER_SETTING_METRIC_ID, user),
            (metrics_registry::STACK_MIGRATION_AUTOMATED_SETTING_METRIC_ID, automated),
        ] {
            let occurrence_count = 1;
            logger
                .log_occurrence(metric_id, occurrence_count, &[event_code][..])
                .await
                .map(|r| {
                    r.unwrap_or_else(|e| warn!("error reported logging metric {metric_id} {e:?}"))
                })
                .unwrap_or_else(|fidl_error| {
                    warn!("error logging metric {metric_id} {fidl_error:?}")
                });
        }
    }
}

#[fuchsia::main]
pub async fn main() {
    info!("running netstack migration service");

    let mut fs = ServiceFs::new();
    let _: &mut ServiceFsDir<'_, _> = fs
        .dir("svc")
        .add_fidl_service(|rs: fnet_migration::ControlRequestStream| {
            rs.map(|req| req.map(ServiceRequest::Control)).left_stream()
        })
        .add_fidl_service(|rs: fnet_migration::StateRequestStream| {
            rs.map(|req| req.map(ServiceRequest::State)).right_stream()
        });
    let _: &mut ServiceFs<_> =
        fs.take_and_serve_directory_handle().expect("failed to take out directory handle");

    let mut migration = Migration::new(DataPersistenceProvider {}, Scheduler {});

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default())
            .expect("failed to serve inspector");
    let inspect_nodes = InspectNodes::new(inspector, &migration);

    let metrics_logger = MetricsLogger::new().await;

    let (desired_version_sender, desired_version_receiver) = mpsc::unbounded();
    let (rollback_state_sender, rollback_state_receiver) = mpsc::unbounded();
    let rollback_state = rollback::State::new(migration.persisted.rollback, migration.current_boot);
    // Update rollback persistence immediately in case the device reboots before
    // the rollback module has time to send an asynchronous update. This is
    // required for correctness if Netstack3 is crashing on startup, or in the
    // following case:
    //
    // 1. Device fails to migrate to Netstack3 and persists
    //    HealthcheckFailures(MAX_FAILED_HEALTHCHECKS), which will force
    //    Netstack2 on subsequent boots.
    // 2. Device reboots into Netstack2, sees that it should be running
    //    Netstack3, and schedules a reboot without clearing the failures.
    // 3. Device reboots back into Netstack3 and sees that it should schedule
    //    a reboot because the persisted failures are above the limit.
    migration.update_rollback_state(rollback_state.persisted()).await;

    Task::spawn(rollback::run(rollback_state, desired_version_receiver, rollback_state_sender))
        .detach();

    enum Action {
        ServiceRequest(Result<ServiceRequest, fidl::Error>),
        LogMetrics,
        UpdateRollbackState(rollback::Persisted),
    }

    let metrics_logging_interval = fuchsia_async::MonotonicDuration::from_hours(1);
    let mut stream: futures::stream::SelectAll<Pin<Box<dyn Stream<Item = Action>>>> =
        futures::stream::SelectAll::new();

    // Always log metrics once on startup then periodically log new values so
    // the aggregation window always contains one sample of the current
    // settings.
    stream.push(Box::pin(Box::new(
        futures::stream::once(futures::future::ready(()))
            .chain(fuchsia_async::Interval::new(metrics_logging_interval))
            .map(|()| Action::LogMetrics),
    )));
    stream.push(Box::pin(Box::new(Box::pin(
        fs.fuse().flatten_unordered(None).map(Action::ServiceRequest),
    ))));
    stream.push(Box::pin(rollback_state_receiver.map(|state| Action::UpdateRollbackState(state))));

    while let Some(action) = stream.next().await {
        match action {
            Action::ServiceRequest(req) => {
                let result = match req {
                    Ok(req) => migration.handle_request(req).await,
                    Err(e) => Err(e),
                };
                // Always update inspector state after handling a request.
                inspect_nodes.update(&migration);

                match desired_version_sender
                    .unbounded_send(migration.persisted.desired_netstack_version())
                {
                    Ok(()) => (),
                    Err(e) => {
                        error!("error sending update to rollback module: {:?}", e);
                    }
                }

                match result {
                    Ok(()) => (),
                    Err(e) => {
                        if !e.is_closed() {
                            error!("error processing FIDL request {:?}", e)
                        }
                    }
                }
            }
            Action::LogMetrics => {
                metrics_logger.log_metrics(&migration).await;
            }
            Action::UpdateRollbackState(new_state) => {
                migration.update_rollback_state(new_state).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use diagnostics_assertions::assert_data_tree;
    use fidl::Peered as _;
    use std::cell::RefCell;
    use std::rc::Rc;
    use test_case::test_case;

    #[derive(Default, Clone)]
    struct InMemory {
        file: Rc<RefCell<Option<Vec<u8>>>>,
    }

    impl InMemory {
        fn with_persisted(p: Persisted) -> Self {
            let mut s = Self::default();
            p.save(s.open_writer().unwrap());
            s
        }
    }

    impl PersistenceProvider for InMemory {
        type Writer = Self;
        type Reader = std::io::Cursor<Vec<u8>>;

        fn open_writer(&mut self) -> std::io::Result<Self::Writer> {
            *self.file.borrow_mut() = Some(Vec::new());
            Ok(self.clone())
        }

        fn open_reader(&self) -> std::io::Result<Self::Reader> {
            self.file
                .borrow()
                .clone()
                .map(std::io::Cursor::new)
                .ok_or(std::io::ErrorKind::NotFound.into())
        }
    }

    impl std::io::Write for InMemory {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let r = self.file.borrow_mut().as_mut().expect("no file open").write(buf);
            r
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct NoCollaborativeReboot;

    impl CollaborativeRebootScheduler for NoCollaborativeReboot {
        async fn schedule(
            &mut self,
            _reason: fpower::CollaborativeRebootReason,
            _cancel: Option<zx::EventPair>,
        ) {
            panic!("unexpectedly attempted to schedule a collaborative reboot");
        }
    }

    #[derive(Default)]
    struct FakeCollaborativeReboot {
        req: Option<zx::EventPair>,
    }

    impl CollaborativeRebootScheduler for FakeCollaborativeReboot {
        async fn schedule(
            &mut self,
            reason: fpower::CollaborativeRebootReason,
            cancel: Option<zx::EventPair>,
        ) {
            assert_eq!(reason, fpower::CollaborativeRebootReason::NetstackMigration);
            let cancel = cancel.expect("cancellation signal must be provided");
            assert_eq!(self.req.replace(cancel), None, "attempted to schedule multiple reboots");
        }
    }

    fn serve_migration<P: PersistenceProvider, CR: CollaborativeRebootScheduler>(
        migration: Migration<P, CR>,
    ) -> (
        impl futures::Future<Output = Migration<P, CR>>,
        fnet_migration::ControlProxy,
        fnet_migration::StateProxy,
    ) {
        let (control, control_server) =
            fidl::endpoints::create_proxy_and_stream::<fnet_migration::ControlMarker>();
        let (state, state_server) =
            fidl::endpoints::create_proxy_and_stream::<fnet_migration::StateMarker>();

        let fut = {
            let control =
                control_server.map(|req| ServiceRequest::Control(req.expect("control error")));
            let state = state_server.map(|req| ServiceRequest::State(req.expect("state error")));
            futures::stream::select(control, state).fold(migration, |mut migration, req| async {
                migration.handle_request(req).await.expect("handling request");
                migration
            })
        };
        (fut, control, state)
    }

    #[test_case(Persisted{
        user: Some(NetstackVersion::Netstack2),
        automated: None,
        rollback: None,
    }; "user_netstack2")]
    #[test_case(Persisted{
        user: Some(NetstackVersion::Netstack3),
        automated: None,
        rollback: None,
    }; "user_netstack3")]
    #[test_case(Persisted{
        user: None,
        automated: None,
        rollback: None,
    }; "none")]
    #[test_case(Persisted{
        user: None,
        automated: Some(NetstackVersion::Netstack2),
        rollback: None,
    }; "automated_netstack2")]
    #[test_case(Persisted{
        user: None,
        automated: Some(NetstackVersion::Netstack3),
        rollback: None,
    }; "automated_netstack3")]
    #[test_case(Persisted{
        user: None,
        automated: None,
        rollback: Some(rollback::Persisted::Success),
    }; "rollback_success")]
    #[test_case(Persisted{
        user: Some(NetstackVersion::Netstack2),
        automated: Some(NetstackVersion::Netstack3),
        rollback: Some(rollback::Persisted::HealthcheckFailures(5)),
    }; "all")]
    #[fuchsia::test(add_test_attr = false)]
    fn persist_save_load(v: Persisted) {
        let mut m = InMemory::default();
        v.save(m.open_writer().unwrap());
        assert_eq!(Persisted::load(m.open_reader().unwrap()), v);
    }

    #[fuchsia::test]
    fn uses_defaults_if_no_persistence() {
        let m = Migration::new(InMemory::default(), NoCollaborativeReboot);
        let Migration {
            current_boot,
            persisted: Persisted { user, automated, rollback: _ },
            persistence: _,
            collaborative_reboot: _,
        } = m;
        assert_eq!(current_boot, DEFAULT_NETSTACK);
        assert_eq!(user, None);
        assert_eq!(automated, None);
    }

    #[test_case(
        None, Some(NetstackVersion::Netstack3), None, NetstackVersion::Netstack3;
        "automated_ns3")]
    #[test_case(
        None, Some(NetstackVersion::Netstack2), None, NetstackVersion::Netstack2;
        "automated_ns2")]
    #[test_case(
        Some(NetstackVersion::Netstack3),
        Some(NetstackVersion::Netstack2),
        Some(rollback::Persisted::HealthcheckFailures(rollback::MAX_FAILED_HEALTHCHECKS)),
        NetstackVersion::Netstack3;
        "user_ns3_override")]
    #[test_case(
        Some(NetstackVersion::Netstack2),
        Some(NetstackVersion::Netstack3),
        Some(rollback::Persisted::Success),
        NetstackVersion::Netstack2;
        "user_ns2_override")]
    #[test_case(
        Some(NetstackVersion::Netstack2),
        None,
        None,
        NetstackVersion::Netstack2; "user_ns2")]
    #[test_case(
        Some(NetstackVersion::Netstack3),
        None,
        None,
        NetstackVersion::Netstack3; "user_ns3")]
    #[test_case(
        None,
        Some(NetstackVersion::Netstack3),
        Some(rollback::Persisted::HealthcheckFailures(rollback::MAX_FAILED_HEALTHCHECKS)),
        NetstackVersion::Netstack2; "rollback_to_ns2")]
    #[test_case(None, None, None, DEFAULT_NETSTACK; "default")]
    #[fuchsia::test]
    async fn get_netstack_version(
        p_user: Option<NetstackVersion>,
        p_automated: Option<NetstackVersion>,
        p_rollback: Option<rollback::Persisted>,
        expect: NetstackVersion,
    ) {
        let m = Migration::new(
            InMemory::with_persisted(Persisted {
                user: p_user,
                automated: p_automated,
                rollback: p_rollback,
            }),
            NoCollaborativeReboot,
        );
        let Migration {
            current_boot,
            persisted: Persisted { user, automated, rollback: _ },
            persistence: _,
            collaborative_reboot: _,
        } = &m;
        assert_eq!(*current_boot, expect);
        assert_eq!(*user, p_user);
        assert_eq!(*automated, p_automated);

        let (serve, _, state) = serve_migration(m);
        let fut = async move {
            let fnet_migration::InEffectVersion { current_boot, user, automated } =
                state.get_netstack_version().await.expect("get netstack version");
            let expect = expect.into();
            let p_user = p_user.map(Into::into);
            let p_automated = p_automated.map(Into::into);
            assert_eq!(current_boot, expect);
            assert_eq!(user, p_user);
            assert_eq!(automated, p_automated);
        };
        let (_, ()): (Migration<_, _>, _) = futures::future::join(serve, fut).await;
    }

    #[derive(Debug, Copy, Clone)]
    enum SetMechanism {
        User,
        Automated,
    }

    #[test_case(SetMechanism::User, NetstackVersion::Netstack2; "set_user_ns2")]
    #[test_case(SetMechanism::User, NetstackVersion::Netstack3; "set_user_ns3")]
    #[test_case(SetMechanism::Automated, NetstackVersion::Netstack2; "set_automated_ns2")]
    #[test_case(SetMechanism::Automated, NetstackVersion::Netstack3; "set_automated_ns3")]
    #[fuchsia::test]
    async fn set_netstack_version(mechanism: SetMechanism, set_version: NetstackVersion) {
        let m = Migration::new(
            InMemory::with_persisted(Default::default()),
            FakeCollaborativeReboot::default(),
        );
        let (serve, control, _) = serve_migration(m);
        let fut = async move {
            let setting = fnet_migration::VersionSetting { version: set_version.into() };
            match mechanism {
                SetMechanism::User => control
                    .set_user_netstack_version(Some(&setting))
                    .await
                    .expect("set user netstack version"),
                SetMechanism::Automated => control
                    .set_automated_netstack_version(Some(&setting))
                    .await
                    .expect("set automated netstack version"),
            }
        };
        let (migration, ()) = futures::future::join(serve, fut).await;

        let validate_versions = |m: &Migration<_, _>, current| {
            let Migration {
                current_boot,
                persisted: Persisted { user, automated, rollback: _ },
                persistence: _,
                collaborative_reboot: _,
            } = m;
            assert_eq!(*current_boot, current);
            match mechanism {
                SetMechanism::User => {
                    assert_eq!(*user, Some(set_version));
                    assert_eq!(*automated, None);
                }
                SetMechanism::Automated => {
                    assert_eq!(*user, None);
                    assert_eq!(*automated, Some(set_version));
                }
            }
        };

        validate_versions(&migration, DEFAULT_NETSTACK);
        let cr_req = &migration.collaborative_reboot.scheduler.req;
        match (mechanism, set_version) {
            (_, NetstackVersion::Netstack3) => {
                assert_eq!(
                    Ok(false),
                    cr_req.as_ref().expect("there should be a request").is_closed()
                )
            }
            _ => assert_eq!(cr_req, &None),
        }

        // Check that the setting was properly persisted.
        let migration =
            Migration::new(migration.persistence, migration.collaborative_reboot.scheduler);
        validate_versions(&migration, set_version);
    }

    #[fuchsia::test]
    async fn update_rollback_state() {
        let mut migration = Migration::new(
            InMemory::with_persisted(Persisted {
                automated: Some(NetstackVersion::Netstack3),
                user: None,
                rollback: None,
            }),
            FakeCollaborativeReboot::default(),
        );

        assert_eq!(migration.current_boot, NetstackVersion::Netstack3);
        assert!(migration.collaborative_reboot.scheduler.req.is_none());

        // The first update shouldn't schedule a reboot because we haven't
        // passed the healthcheck threshold yet.
        migration.update_rollback_state(rollback::Persisted::HealthcheckFailures(1)).await;
        assert_matches!(
            migration.persisted.rollback,
            Some(rollback::Persisted::HealthcheckFailures(1))
        );
        assert!(migration.collaborative_reboot.scheduler.req.is_none());

        // This second update should schedule a reboot because we've passed
        // the healthcheck limit and want to roll back to Netstack2.
        migration
            .update_rollback_state(rollback::Persisted::HealthcheckFailures(
                rollback::MAX_FAILED_HEALTHCHECKS,
            ))
            .await;
        assert_matches!(
            migration.persisted.rollback,
            Some(rollback::Persisted::HealthcheckFailures(rollback::MAX_FAILED_HEALTHCHECKS))
        );
        assert_eq!(
            migration
                .collaborative_reboot
                .scheduler
                .req
                .as_ref()
                .expect("reboot was not scheduled")
                .is_closed()
                .unwrap(),
            false
        );

        // This emulates seeing a healthcheck success before rebooting, in which
        // case we should see the reboot get canceled.
        migration.update_rollback_state(rollback::Persisted::Success).await;
        assert_matches!(migration.persisted.rollback, Some(rollback::Persisted::Success));
        assert!(migration
            .collaborative_reboot
            .scheduler
            .req
            .as_ref()
            .unwrap()
            .is_closed()
            .unwrap());

        // Ensure that the changes were persisted successfully.
        let migration =
            Migration::new(migration.persistence, migration.collaborative_reboot.scheduler);
        assert_matches!(migration.persisted.rollback, Some(rollback::Persisted::Success));
    }

    #[test_case(SetMechanism::User, Some(NetstackVersion::Netstack2), true)]
    #[test_case(SetMechanism::User, Some(NetstackVersion::Netstack3), false)]
    #[test_case(SetMechanism::User, None, false)]
    #[test_case(SetMechanism::Automated, Some(NetstackVersion::Netstack2), true)]
    #[test_case(SetMechanism::Automated, Some(NetstackVersion::Netstack3), false)]
    #[test_case(SetMechanism::Automated, None, true)]
    #[fuchsia::test]
    async fn cancel_collaborative_reboot(
        mechanism: SetMechanism,
        version: Option<NetstackVersion>,
        expect_canceled: bool,
    ) {
        let migration = Migration::new(
            InMemory::with_persisted(Persisted { user: None, automated: None, rollback: None }),
            FakeCollaborativeReboot::default(),
        );

        // Start of by updating the automated setting to Netstack3; this ensures
        // their is a pending request to cancel.
        let (serve, control, _) = serve_migration(migration);
        let fut = async move {
            control
                .set_automated_netstack_version(Some(&fnet_migration::VersionSetting {
                    version: fnet_migration::NetstackVersion::Netstack3,
                }))
                .await
                .expect("set automated netstack version");
        };
        let (migration, ()) = futures::future::join(serve, fut).await;
        let cancel = migration
            .collaborative_reboot
            .scheduler
            .req
            .as_ref()
            .expect("there should be a request");
        assert_eq!(Ok(false), cancel.is_closed());

        // Update the setting based on the test parameters
        let (serve, control, _) = serve_migration(migration);
        let fut = async move {
            let setting = version.map(|v| fnet_migration::VersionSetting { version: v.into() });
            match mechanism {
                SetMechanism::User => control
                    .set_user_netstack_version(setting.as_ref())
                    .await
                    .expect("set user netstack version"),
                SetMechanism::Automated => control
                    .set_automated_netstack_version(setting.as_ref())
                    .await
                    .expect("set automated netstack version"),
            }
        };
        let (migration, ()) = futures::future::join(serve, fut).await;

        let cancel = migration
            .collaborative_reboot
            .scheduler
            .req
            .as_ref()
            .expect("there should be a request");
        assert_eq!(Ok(expect_canceled), cancel.is_closed());
    }

    #[test_case(SetMechanism::User)]
    #[test_case(SetMechanism::Automated)]
    #[fuchsia::test]
    async fn clear_netstack_version(mechanism: SetMechanism) {
        const PREVIOUS_VERSION: NetstackVersion = NetstackVersion::Netstack2;
        let m = Migration::new(
            InMemory::with_persisted(Persisted {
                user: Some(PREVIOUS_VERSION),
                automated: Some(PREVIOUS_VERSION),
                rollback: None,
            }),
            NoCollaborativeReboot,
        );
        let (serve, control, _) = serve_migration(m);
        let fut = async move {
            match mechanism {
                SetMechanism::User => control
                    .set_user_netstack_version(None)
                    .await
                    .expect("set user netstack version"),
                SetMechanism::Automated => control
                    .set_automated_netstack_version(None)
                    .await
                    .expect("set automated netstack version"),
            }
        };
        let (migration, ()) = futures::future::join(serve, fut).await;

        let validate_versions = |m: &Migration<_, _>| {
            let Migration {
                current_boot,
                persisted: Persisted { user, automated, rollback: _ },
                persistence: _,
                collaborative_reboot: _,
            } = m;
            assert_eq!(*current_boot, PREVIOUS_VERSION);
            match mechanism {
                SetMechanism::User => {
                    assert_eq!(*user, None);
                    assert_eq!(*automated, Some(PREVIOUS_VERSION));
                }
                SetMechanism::Automated => {
                    assert_eq!(*user, Some(PREVIOUS_VERSION));
                    assert_eq!(*automated, None);
                }
            }
        };

        validate_versions(&migration);
        // Check that the setting was properly persisted.
        let migration =
            Migration::new(migration.persistence, migration.collaborative_reboot.scheduler);
        validate_versions(&migration);
    }

    #[fuchsia::test]
    fn inspect() {
        let mut m = Migration::new(
            InMemory::with_persisted(Persisted {
                user: Some(NetstackVersion::Netstack2),
                automated: Some(NetstackVersion::Netstack3),
                rollback: None,
            }),
            NoCollaborativeReboot,
        );
        let inspector = fuchsia_inspect::component::inspector();
        let nodes = InspectNodes::new(inspector, &m);
        assert_data_tree!(inspector,
            root: {
                current_boot: 2u64,
                user_setting: 2u64,
                automated_setting: 3u64,
            }
        );

        m.persisted =
            Persisted { user: None, automated: Some(NetstackVersion::Netstack2), rollback: None };
        nodes.update(&m);
        assert_data_tree!(inspector,
            root: {
                current_boot: 2u64,
                user_setting: 0u64,
                automated_setting: 2u64,
            }
        );
    }

    #[test_case::test_matrix(
    [
        (NetstackVersion::Netstack2, metrics_registry::StackMigrationCurrentBootMetricDimensionNetstackVersion::Netstack2),
        (NetstackVersion::Netstack3, metrics_registry::StackMigrationCurrentBootMetricDimensionNetstackVersion::Netstack3),
    ],
    [
        (None, metrics_registry::StackMigrationUserSettingMetricDimensionNetstackVersion::NoSelection),
        (Some(NetstackVersion::Netstack2), metrics_registry::StackMigrationUserSettingMetricDimensionNetstackVersion::Netstack2),
        (Some(NetstackVersion::Netstack3), metrics_registry::StackMigrationUserSettingMetricDimensionNetstackVersion::Netstack3),
    ],
    [
        (None, metrics_registry::StackMigrationAutomatedSettingMetricDimensionNetstackVersion::NoSelection),
        (Some(NetstackVersion::Netstack2), metrics_registry::StackMigrationAutomatedSettingMetricDimensionNetstackVersion::Netstack2),
        (Some(NetstackVersion::Netstack3), metrics_registry::StackMigrationAutomatedSettingMetricDimensionNetstackVersion::Netstack3),
    ]
    )]
    #[fuchsia::test]
    async fn metrics_logger(
        current_boot: (
            NetstackVersion,
            metrics_registry::StackMigrationCurrentBootMetricDimensionNetstackVersion,
        ),
        user: (
            Option<NetstackVersion>,
            metrics_registry::StackMigrationUserSettingMetricDimensionNetstackVersion,
        ),
        automated: (
            Option<NetstackVersion>,
            metrics_registry::StackMigrationAutomatedSettingMetricDimensionNetstackVersion,
        ),
    ) {
        let (current_boot, current_boot_expect) = current_boot;
        let (user, user_expect) = user;
        let (automated, automated_expect) = automated;
        let mut m = Migration::new(
            InMemory::with_persisted(Persisted { user, automated, rollback: None }),
            NoCollaborativeReboot,
        );
        m.current_boot = current_boot;
        let (logger, mut logger_stream) =
            fidl::endpoints::create_proxy_and_stream::<fmetrics::MetricEventLoggerMarker>();

        let metrics_logger = MetricsLogger { logger: Some(logger) };

        let ((), ()) = futures::future::join(metrics_logger.log_metrics(&m), async {
            let expect = [
                (
                    metrics_registry::STACK_MIGRATION_CURRENT_BOOT_METRIC_ID,
                    current_boot_expect.as_event_code(),
                ),
                (
                    metrics_registry::STACK_MIGRATION_USER_SETTING_METRIC_ID,
                    user_expect.as_event_code(),
                ),
                (
                    metrics_registry::STACK_MIGRATION_AUTOMATED_SETTING_METRIC_ID,
                    automated_expect.as_event_code(),
                ),
            ];
            for (id, ev) in expect {
                let (metric, occurences, codes, responder) = logger_stream
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .into_log_occurrence()
                    .expect("bad request");
                assert_eq!(metric, id);
                assert_eq!(occurences, 1);
                assert_eq!(codes, vec![ev]);
                responder.send(Ok(())).unwrap();
            }
        })
        .await;
    }
}
