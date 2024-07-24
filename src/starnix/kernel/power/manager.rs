// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::power::{listener, SuspendState, SuspendStats};
use crate::task::CurrentTask;

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex as StdMutex};

use anyhow::{anyhow, Context};
use async_utils::hanging_get::client::HangingGetStream;
use fidl::endpoints::create_sync_proxy;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_sync};
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use futures::StreamExt;
use once_cell::sync::OnceCell;
use rand::distributions::Alphanumeric;
use rand::Rng;
use starnix_logging::{log_error, log_warn};
use starnix_sync::{Mutex, MutexGuard};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error};
use {
    fidl_fuchsia_power_broker as fbroker, fidl_fuchsia_power_suspend as fsuspend,
    fidl_fuchsia_power_system as fsystem, fidl_fuchsia_session_power as fpower,
    fuchsia_inspect as inspect, fuchsia_zircon as zx,
};

#[derive(Debug)]
struct PowerElement {
    element_proxy: fbroker::ElementControlSynchronousProxy,
    lessor_proxy: fbroker::LessorSynchronousProxy,
    level_proxy: Option<fbroker::CurrentLevelSynchronousProxy>,
}

// String keys used for various suspend events.  We should try to keep these
// keys in sync across binaries.
const SUSPEND_FAILED_AT: &str = "failed_at_ns";
const SUSPEND_ATTEMPTED_AT: &str = "attempted_at_ns";
const SUSPEND_RESUMED_AT: &str = "resumed_at_ns";
const SUSPEND_REQUESTED_STATE: &str = "requested_power_state";

/// Manager for suspend and resume.
#[derive(Default)]
pub struct SuspendResumeManager {
    /// Power Mode power element is owned and registered by Starnix kernel. This power element is
    /// added in the power topology as a dependent on Application Activity element that is owned by
    /// the SAG.
    ///
    /// After Starnix boots, a power-on lease will be created and retained.
    ///
    /// When it need to suspend, Starnix should create another lease for the suspend state and
    /// release the power-on lease.
    ///
    /// The power level will only be changed to the requested level when all elements in the
    /// topology can maintain the minimum power equilibrium in the lease.
    ///
    /// | Power Mode        | Level |
    /// | ----------------- | ----- |
    /// | On                | 4     |
    /// | Suspend-to-Idle   | 3     |
    /// | Standby           | 2     |
    /// | Suspend-to-RAM    | 1     |
    /// | Suspend-to-Disk   | 0     |
    power_mode: OnceCell<PowerElement>,

    // The mutable state of [SuspendResumeManager].
    inner: Mutex<SuspendResumeManagerInner>,
}
pub(super) static STARNIX_POWER_ON_LEVEL: fbroker::PowerLevel = 4;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum SuspendResult {
    /// Indicates suspension was successful.
    ///
    /// Note that a successful suspension result may be returned _after_ resuming
    /// from a suspend. Observers may not assume that this result will be observed
    /// before the system actually suspends.
    Success,
    Failure,
}

#[derive(Default)]
struct SuspendWaiter {
    cond_var: Condvar,
    result: StdMutex<Option<SuspendResult>>,
}

impl SuspendWaiter {
    fn new() -> Arc<Self> {
        Arc::new(SuspendWaiter::default())
    }

    fn wait(self: Arc<Self>) -> SuspendResult {
        let guard = self.result.lock().unwrap();
        self.cond_var
            .wait_while(guard, |result| result.is_none())
            .unwrap()
            .expect("result is set before being notified")
    }
}

/// Manager for suspend and resume.
pub struct SuspendResumeManagerInner {
    /// The suspend counters and gauges.
    suspend_stats: SuspendStats,
    sync_on_suspend_enabled: bool,
    /// Lease control channel to hold the system power state as active.
    lease_control_channel: Option<zx::Channel>,

    suspend_waiter: Option<Arc<SuspendWaiter>>,
    inspect_node: BoundedListNode,
}

/// The inspect node ring buffer will keep at most this many entries.
const INSPECT_RING_BUFFER_CAPACITY: usize = 128;

impl Default for SuspendResumeManagerInner {
    fn default() -> Self {
        Self {
            inspect_node: BoundedListNode::new(
                inspect::component::inspector().root().create_child("suspend_events"),
                INSPECT_RING_BUFFER_CAPACITY,
            ),
            suspend_stats: Default::default(),
            sync_on_suspend_enabled: Default::default(),
            lease_control_channel: Default::default(),
            suspend_waiter: Default::default(),
        }
    }
}

pub type SuspendResumeManagerHandle = Arc<SuspendResumeManager>;

impl SuspendResumeManager {
    /// Locks and returns the inner state of the manager.
    fn lock(&self) -> MutexGuard<'_, SuspendResumeManagerInner> {
        self.inner.lock()
    }

    /// Power on the PowerMode element and start listening to the suspend stats updates.
    pub fn init(
        self: &SuspendResumeManagerHandle,
        system_task: &CurrentTask,
    ) -> Result<(), anyhow::Error> {
        let activity_governor = connect_to_protocol_sync::<fsystem::ActivityGovernorMarker>()?;
        let handoff = system_task
            .kernel()
            .connect_to_protocol_at_container_svc::<fpower::HandoffMarker>()?
            .into_sync_proxy();
        self.init_power_element(&activity_governor, &handoff, system_task)?;
        listener::init_listener(self, &activity_governor, system_task);
        self.init_stats_watcher(system_task);
        Ok(())
    }

    fn init_power_element(
        self: &SuspendResumeManagerHandle,
        activity_governor: &fsystem::ActivityGovernorSynchronousProxy,
        handoff: &fpower::HandoffSynchronousProxy,
        system_task: &CurrentTask,
    ) -> Result<(), anyhow::Error> {
        let topology = connect_to_protocol_sync::<fbroker::TopologyMarker>()?;

        // Create the PowerMode power element depending on the Application Activity of SAG.
        let power_elements = activity_governor
            .get_power_elements(zx::Time::INFINITE)
            .context("cannot get Activity Governor element from SAG")?;
        if let Some(Some(application_activity_token)) = power_elements
            .application_activity
            .map(|application_activity| application_activity.assertive_dependency_token)
        {
            // TODO(https://fxbug.dev/316023943): also depend on execution_resume_latency after implemented.
            let power_levels: Vec<u8> = (0..=STARNIX_POWER_ON_LEVEL).collect();
            let (element_control, element_control_server_end) =
                create_sync_proxy::<fbroker::ElementControlMarker>();
            let (lessor, lessor_server_end) = create_sync_proxy::<fbroker::LessorMarker>();
            let (current_level, current_level_server_end) =
                create_sync_proxy::<fbroker::CurrentLevelMarker>();
            let (required_level, required_level_server_end) =
                create_sync_proxy::<fbroker::RequiredLevelMarker>();
            let level_control_channels = fbroker::LevelControlChannels {
                current: current_level_server_end,
                required: required_level_server_end,
            };
            topology
                .add_element(
                    fbroker::ElementSchema {
                        element_name: Some("starnix-power-mode".into()),
                        initial_current_level: Some(0),
                        valid_levels: Some(power_levels),
                        dependencies: Some(vec![fbroker::LevelDependency {
                            dependency_type: fbroker::DependencyType::Assertive,
                            dependent_level: STARNIX_POWER_ON_LEVEL,
                            requires_token: application_activity_token,
                            requires_level_by_preference: vec![
                                fsystem::ApplicationActivityLevel::Active.into_primitive(),
                            ],
                        }]),
                        element_control: Some(element_control_server_end),
                        lessor_channel: Some(lessor_server_end),
                        level_control_channels: Some(level_control_channels),
                        ..Default::default()
                    },
                    zx::Time::INFINITE,
                )?
                .map_err(|e| anyhow!("PowerBroker::AddElementError({e:?})"))?;

            // Power on by holding a lease.
            let power_on_control = lessor
                .lease(STARNIX_POWER_ON_LEVEL, zx::Time::INFINITE)?
                .map_err(|e| anyhow!("PowerBroker::LeaseError({e:?})"))?
                .into_channel();
            self.lock().lease_control_channel = Some(power_on_control);

            self.power_mode
                .set(PowerElement {
                    element_proxy: element_control,
                    lessor_proxy: lessor,
                    level_proxy: Some(current_level),
                })
                .expect("Power Mode should be uninitialized");

            let self_ref = self.clone();
            system_task.kernel().kthreads.spawn(move |_, _| {
                while let Ok(Ok(level)) = required_level.watch(zx::Time::INFINITE) {
                    if let Err(e) = self_ref
                        .power_mode()
                        .expect("Starnix should have a power mode")
                        .level_proxy
                        .as_ref()
                        .expect("Starnix power mode should have a current level proxy")
                        .update(level, zx::Time::INFINITE)
                    {
                        log_warn!("Failed to update current level: {e:?}");
                        break;
                    }
                }
            });

            // We may not have a session manager to take a lease from in tests.
            match handoff.take(zx::Time::INFINITE) {
                Ok(parent_lease) => {
                    let parent_lease = parent_lease.map_err(|e| {
                        anyhow!("Failed to take lessor and lease from parent: {e:?}")
                    })?;
                    drop(parent_lease)
                }
                Err(e) => {
                    if e.is_closed() {
                        log_warn!("Failed to send the fuchsia.session.power/Handoff.Take request. Assuming no Handoff protocol exists and moving on...");
                    } else {
                        return Err(e).context("Handoff::Take");
                    }
                }
            }
        };

        Ok(())
    }

    fn update_stats(&self, stats: fsuspend::SuspendStats) {
        let stats_guard = &mut self.lock().suspend_stats;

        // Only update the stats if the new stats moves forward.
        let success_count = stats.success_count.unwrap_or_default();
        if stats_guard.success_count > success_count {
            return;
        }
        let fail_count = stats.fail_count.unwrap_or_default();
        if stats_guard.fail_count > fail_count {
            return;
        }

        stats_guard.success_count = stats.success_count.unwrap_or_default();
        stats_guard.fail_count = stats.fail_count.unwrap_or_default();
        stats_guard.last_time_in_sleep =
            zx::Duration::from_millis(stats.last_time_in_suspend.unwrap_or_default());
        stats_guard.last_time_in_suspend_operations =
            zx::Duration::from_millis(stats.last_time_in_suspend_operations.unwrap_or_default());
    }

    fn init_stats_watcher(self: &SuspendResumeManagerHandle, system_task: &CurrentTask) {
        let self_ref = self.clone();
        system_task.kernel().kthreads.spawn_future(async move {
            // Start listening to the suspend stats updates
            let stats_proxy = connect_to_protocol::<fsuspend::StatsMarker>()
                .expect("connection to fuchsia.power.suspend.Stats");
            let mut stats_stream = HangingGetStream::new(stats_proxy, fsuspend::StatsProxy::watch);
            while let Some(stats) = stats_stream.next().await {
                match stats {
                    Ok(stats) => self_ref.update_stats(stats),
                    Err(e) => {
                        log_error!("stats watcher got an error: {}", e);
                        break;
                    }
                }
            }
        });
    }

    fn power_mode(&self) -> Result<&PowerElement, Errno> {
        match self.power_mode.get() {
            Some(p) => Ok(p),
            None => error!(EAGAIN, "power-mode element is not initialized"),
        }
    }

    /// Gets the suspend statistics.
    pub fn suspend_stats(&self) -> SuspendStats {
        self.lock().suspend_stats.clone()
    }

    /// Get the contents of the power "sync_on_suspend" file in the power
    /// filesystem.  True will cause `1` to be reported, and false will cause
    /// `0` to be reported.
    pub fn sync_on_suspend_enabled(&self) -> bool {
        self.lock().sync_on_suspend_enabled.clone()
    }

    /// Get the contents of the power "sync_on_suspend" file in the power
    /// filesystem.  See also [sync_on_suspend_enabled].
    pub fn set_sync_on_suspend(&self, enable: bool) {
        self.lock().sync_on_suspend_enabled = enable;
    }

    /// Returns the supported suspend states.
    pub fn suspend_states(&self) -> HashSet<SuspendState> {
        // TODO(b/326470421): Remove the hardcoded supported state.
        HashSet::from([SuspendState::Ram, SuspendState::Idle])
    }

    /// Sets the power level to `level`.
    pub(super) fn update_power_level(&self, level: fbroker::PowerLevel) -> Result<(), Errno> {
        let power_mode = self.power_mode()?;
        // Before the old lease is dropped, a new lease must be created to transit to the
        // new level. This ensures a smooth transition without going back to the initial
        // power level.
        match power_mode.lessor_proxy.lease(level, zx::Time::INFINITE) {
            Ok(Ok(lease_client)) => {
                // Wait until the lease is satisfied.
                let lease_control = lease_client.into_sync_proxy();
                let mut lease_status = fbroker::LeaseStatus::Unknown;
                while lease_status != fbroker::LeaseStatus::Satisfied {
                    lease_status = lease_control
                        .watch_status(lease_status, zx::Time::INFINITE)
                        .map_err(|_| errno!(EINVAL))?;
                }
                self.lock().lease_control_channel = Some(lease_control.into_channel());
            }
            Ok(Err(err)) => {
                return error!(EINVAL, format!("power broker lease error {:?}", err));
            }
            Err(err) => {
                return error!(EINVAL, format!("power broker lease fidl error {err}"));
            }
        }

        match power_mode
            .level_proxy
            .as_ref()
            .expect("Starnix PowerMode should have power level proxy")
            .update(level, zx::Time::INFINITE)
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(err)) => error!(EINVAL, format!("power level update error {:?}", err)),
            Err(err) => error!(EINVAL, format!("power level update fidl error {err}")),
        }
    }

    fn wait_for_power_level(&self, level: fbroker::PowerLevel) -> Result<(), Errno> {
        // Create power element status stream
        let (element_status, element_status_server) = create_sync_proxy::<fbroker::StatusMarker>();
        self.power_mode()?
            .element_proxy
            .open_status_channel(element_status_server)
            .map_err(|e| errno!(EINVAL, format!("Status channel failed to open: {e}")))?;
        while element_status
            .watch_power_level(zx::Time::INFINITE)
            .map_err(|err| errno!(EINVAL, format!("power element status watch error {err}")))?
            .map_err(|err| {
                errno!(EINVAL, format!("power element status watch fidl error {:?}", err))
            })?
            != level
        {}
        Ok(())
    }

    /// Notify all waiters of the suspension `result`.
    pub(super) fn notify_suspension(&self, result: SuspendResult) {
        let waiters = std::mem::take(&mut self.lock().suspend_waiter);
        for waiter in waiters.into_iter() {
            let mut guard = waiter.result.lock().unwrap();
            let prev = guard.replace(result);
            debug_assert_eq!(prev, None, "waiter should only be notified once");
            // We should only have a single thread blocked per waiter.
            waiter.cond_var.notify_one();
        }
    }

    /// Executed on suspend.
    pub fn suspend(&self, state: SuspendState) -> Result<(), Errno> {
        self.lock().inspect_node.add_entry(|node| {
            node.record_int(SUSPEND_ATTEMPTED_AT, zx::Time::get_monotonic().into_nanos());
            node.record_string(SUSPEND_REQUESTED_STATE, state.to_str());
        });

        let waiter = SuspendWaiter::new();
        let prev = self.lock().suspend_waiter.replace(Arc::clone(&waiter));
        debug_assert!(prev.is_none(), "Should not have concurrent suspend attempts");

        self.update_power_level(state.into()).inspect_err(|_| {
            self.lock().suspend_waiter.take();
        })?;

        // Starnix will wait here on suspend.
        let suspend_result = waiter.wait();

        // Synchronously update the stats after performing suspend so that a later
        // query of stats is guaranteed to reflect the current suspend operation.
        let stats_proxy = connect_to_protocol_sync::<fsuspend::StatsMarker>()
            .expect("connection to fuchsia.power.suspend.Stats");
        match stats_proxy.watch(zx::Time::INFINITE) {
            Ok(stats) => self.update_stats(stats),
            Err(e) => log_warn!("failed to update stats after suspend: {e:?}"),
        }

        // Use the same "now" for all subsequent stats.
        let now = zx::Time::get_monotonic();

        match suspend_result {
            SuspendResult::Success => self.wait_for_power_level(STARNIX_POWER_ON_LEVEL)?,
            SuspendResult::Failure => {
                self.lock().inspect_node.add_entry(|node| {
                    node.record_int(SUSPEND_FAILED_AT, now.into_nanos());
                });
                return error!(EINVAL, format!("failed to suspend at ns: {}", &now.into_nanos()));
            }
        }

        self.lock().inspect_node.add_entry(|node| {
            node.record_int(SUSPEND_RESUMED_AT, now.into_nanos());
        });

        Ok(())
    }
}

/// A power lease to keep the system awake.
///
/// The lease is armed when the `activate` method is called, and it is released/transferred when
/// the `take_lease` method is called.
///
/// The wake-lease PE is a dependency of the SAG `WakeHandling` PE that is responsible for keeping
/// the system awake.
///
/// This is useful for syscalls that need to keep the system awake for a period of time, such as
/// `EPOLLWAKEUP` event in epoll.
pub struct WakeLease {
    name: String,
    element: OnceCell<PowerElement>,
    lease: Mutex<Option<zx::Channel>>,
}

impl WakeLease {
    pub fn new(name: &str) -> Self {
        let random_string: String =
            rand::thread_rng().sample_iter(&Alphanumeric).take(8).map(char::from).collect();
        Self {
            name: format!("{}-{}", name, random_string),
            element: Default::default(),
            lease: Default::default(),
        }
    }

    fn element(&self) -> Result<&PowerElement, Errno> {
        self.element.get_or_try_init(|| {
            let activity_governor = connect_to_protocol_sync::<fsystem::ActivityGovernorMarker>()
                .map_err(|_| errno!(EINVAL, "Failed to connect to SAG"))?;
            let topology = connect_to_protocol_sync::<fbroker::TopologyMarker>()
                .map_err(|_| errno!(EINVAL, "Failed to connect to Power Topology"))?;

            let power_elements =
                activity_governor.get_power_elements(zx::Time::INFINITE).map_err(|e| {
                    errno!(EINVAL, format!("cannot get Activity Governor element from SAG: {e}"))
                })?;
            let Some(active_wake_token) = power_elements
                .wake_handling
                .and_then(|wake_handling| wake_handling.assertive_dependency_token)
            else {
                return Err(errno!(EINVAL, "No active dependency token in SAG Wake Handling PE"));
            };
            let power_levels: Vec<u8> =
                (0..=fbroker::BinaryPowerLevel::On.into_primitive()).collect();
            let (lessor_proxy, lessor_server_end) = create_sync_proxy::<fbroker::LessorMarker>();
            let (element_proxy, element_server_end) =
                create_sync_proxy::<fbroker::ElementControlMarker>();
            topology
                .add_element(
                    fbroker::ElementSchema {
                        element_name: Some(format!("starnix-wake-lock-{}", self.name)),
                        initial_current_level: Some(
                            fbroker::BinaryPowerLevel::Off.into_primitive(),
                        ),
                        valid_levels: Some(power_levels),
                        dependencies: Some(vec![fbroker::LevelDependency {
                            dependency_type: fbroker::DependencyType::Assertive,
                            dependent_level: fbroker::BinaryPowerLevel::On.into_primitive(),
                            requires_token: active_wake_token,
                            requires_level_by_preference: vec![
                                fsystem::WakeHandlingLevel::Active.into_primitive()
                            ],
                        }]),
                        lessor_channel: Some(lessor_server_end),
                        element_control: Some(element_server_end),
                        ..Default::default()
                    },
                    zx::Time::INFINITE,
                )
                .map_err(|e| errno!(EINVAL, format!("PowerBroker::AddElement fidl error ({e:?})")))?
                .map_err(|e| errno!(EINVAL, format!("PowerBroker::AddElementError ({e:?})")))?;
            Ok(PowerElement { element_proxy, lessor_proxy, level_proxy: None })
        })
    }

    /// Activate the wake lease.
    pub fn activate(&self) -> Result<(), Errno> {
        let element = self.element()?;
        let mut guard = self.lease.lock();
        if guard.is_none() {
            *guard = Some(
                element
                    .lessor_proxy
                    .lease(fbroker::BinaryPowerLevel::On.into_primitive(), zx::Time::INFINITE)
                    .map_err(|e| errno!(EINVAL, format!("PowerBroker::Lease fidl error ({e:?})")))?
                    .map_err(|e| errno!(EINVAL, format!("PowerBroker::LeaseError ({e:?})")))?
                    .into_channel(),
            );
        }
        Ok(())
    }
}

impl WakeLeaseInterlockOps for WakeLease {
    fn take_lease(&self) -> Option<zx::Channel> {
        self.lease.lock().take()
    }
}

/// `WakeLeaseInterlockOps` is a trait that defines the interface for handling a wake lease in a
/// interlock manner.
///
/// Interlock mechanism is used to ensure that the successor lease is activated before the
/// predecessor lease is dropped. This is important to ensure that any common dependencies of the
/// predecessor and successor leases remain actively claimed across a transfer of flow control.
pub trait WakeLeaseInterlockOps {
    /// Transfer the active wake lease to the caller.
    ///
    /// Ignoring the returned Channel means dropping the wake lease.
    fn take_lease(&self) -> Option<zx::Channel>;
}

pub trait OnWakeOps: Send + Sync {
    fn on_wake(&self, current_task: &CurrentTask, baton_lease: &zx::Channel);
}
