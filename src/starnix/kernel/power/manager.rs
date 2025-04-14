// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::power::{SuspendState, SuspendStats};
use crate::task::CurrentTask;
use crate::vfs::EpollKey;

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_inspect::ArrayProperty;
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use starnix_logging::log_warn;
use starnix_sync::{Mutex, MutexGuard};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error};
use zx::{HandleBased, Peered};
use {
    fidl_fuchsia_power_observability as fobs, fidl_fuchsia_session_power as fpower,
    fidl_fuchsia_starnix_runner as frunner, fuchsia_inspect as inspect,
};

/// Manager for suspend and resume.
#[derive(Default)]
pub struct SuspendResumeManager {
    // The mutable state of [SuspendResumeManager].
    inner: Mutex<SuspendResumeManagerInner>,
}

/// Manager for suspend and resume.
pub struct SuspendResumeManagerInner {
    /// The suspend counters and gauges.
    suspend_stats: SuspendStats,
    sync_on_suspend_enabled: bool,

    inspect_node: BoundedListNode,

    /// The currently active wake locks in the system. If non-empty, this prevents
    /// the container from suspending.
    active_locks: HashSet<String>,
    inactive_locks: HashSet<String>,

    /// The currently active EPOLLWAKEUPs in the system. If non-empty, this prevents
    /// the container from suspending.
    active_epolls: HashSet<EpollKey>,

    /// The event pair that is passed to the Starnix runner so it can observe whether
    /// or not any wake locks are active before completing a suspend operation.
    active_lock_reader: zx::EventPair,

    /// The event pair that is used by the Starnix kernel to signal when there are
    /// active wake locks in the container. Note that the peer of the writer is the
    /// object that is signaled.
    active_lock_writer: zx::EventPair,
}

/// The inspect node ring buffer will keep at most this many entries.
const INSPECT_RING_BUFFER_CAPACITY: usize = 128;

impl Default for SuspendResumeManagerInner {
    fn default() -> Self {
        let (active_lock_reader, active_lock_writer) = zx::EventPair::create();
        Self {
            inspect_node: BoundedListNode::new(
                inspect::component::inspector().root().create_child("suspend_events"),
                INSPECT_RING_BUFFER_CAPACITY,
            ),
            suspend_stats: Default::default(),
            sync_on_suspend_enabled: Default::default(),
            active_locks: Default::default(),
            inactive_locks: Default::default(),
            active_epolls: Default::default(),
            active_lock_reader,
            active_lock_writer,
        }
    }
}

impl SuspendResumeManagerInner {
    /// Signals whether or not there are currently any active wake locks in the kernel.
    fn signal_wake_events(&mut self) {
        let (clear_mask, set_mask) =
            if self.active_locks.is_empty() && self.active_epolls.is_empty() {
                (zx::Signals::EVENT_SIGNALED, zx::Signals::empty())
            } else {
                (zx::Signals::empty(), zx::Signals::EVENT_SIGNALED)
            };
        self.active_lock_writer.signal_peer(clear_mask, set_mask).expect("Failed to signal peer");
    }
}

pub type SuspendResumeManagerHandle = Arc<SuspendResumeManager>;

impl SuspendResumeManager {
    /// Locks and returns the inner state of the manager.
    pub fn lock(&self) -> MutexGuard<'_, SuspendResumeManagerInner> {
        self.inner.lock()
    }

    /// Power on the PowerMode element and start listening to the suspend stats updates.
    pub fn init(
        self: &SuspendResumeManagerHandle,
        system_task: &CurrentTask,
    ) -> Result<(), anyhow::Error> {
        let handoff = system_task
            .kernel()
            .connect_to_protocol_at_container_svc::<fpower::HandoffMarker>()?
            .into_sync_proxy();
        match handoff.take(zx::MonotonicInstant::INFINITE) {
            Ok(parent_lease) => {
                let parent_lease = parent_lease
                    .map_err(|e| anyhow!("Failed to take lessor and lease from parent: {e:?}"))?;
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
        Ok(())
    }

    /// Adds a wake lock `name` to the active wake locks.
    pub fn add_lock(&self, name: &str) -> bool {
        let mut state = self.lock();
        let res = state.active_locks.insert(String::from(name));
        state.signal_wake_events();
        res
    }

    /// Removes a wake lock `name` from the active wake locks.
    pub fn remove_lock(&self, name: &str) -> bool {
        let mut state = self.lock();
        let res = state.active_locks.remove(name);
        if !res {
            return false;
        }

        state.inactive_locks.insert(String::from(name));
        state.signal_wake_events();
        res
    }

    /// Adds a wake lock `key` to the active epoll wake locks.
    pub fn add_epoll(&self, key: EpollKey) {
        let mut state = self.lock();
        state.active_epolls.insert(key);
        state.signal_wake_events();
    }

    /// Removes a wake lock `key` from the active epoll wake locks.
    pub fn remove_epoll(&self, key: EpollKey) {
        let mut state = self.lock();
        state.active_epolls.remove(&key);
        state.signal_wake_events();
    }

    pub fn active_wake_locks(&self) -> Vec<String> {
        Vec::from_iter(self.lock().active_locks.clone())
    }

    pub fn inactive_wake_locks(&self) -> Vec<String> {
        Vec::from_iter(self.lock().inactive_locks.clone())
    }

    /// Returns a duplicate handle to the `EventPair` that is signaled when wake
    /// locks are active.
    pub fn duplicate_lock_event(&self) -> zx::EventPair {
        let state = self.lock();
        state
            .active_lock_reader
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .expect("Failed to duplicate handle")
    }

    /// Gets the suspend statistics.
    pub fn suspend_stats(&self) -> SuspendStats {
        self.lock().suspend_stats.clone()
    }

    pub fn update_suspend_stats<UpdateFn>(&self, update: UpdateFn)
    where
        UpdateFn: FnOnce(&mut SuspendStats),
    {
        let stats_guard = &mut self.lock().suspend_stats;
        update(stats_guard);
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
        HashSet::from([SuspendState::Idle])
    }

    pub fn suspend(&self, state: SuspendState) -> Result<(), Errno> {
        let suspend_start_time = zx::BootInstant::get();

        self.lock().inspect_node.add_entry(|node| {
            node.record_int(fobs::SUSPEND_ATTEMPTED_AT, suspend_start_time.clone().into_nanos());
            node.record_string(fobs::SUSPEND_REQUESTED_STATE, state.to_string());
        });

        let manager = connect_to_protocol_sync::<frunner::ManagerMarker>()
            .expect("Failed to connect to manager");
        fuchsia_trace::duration!(c"power", c"suspend_container:fidl");
        match manager.suspend_container(
            frunner::ManagerSuspendContainerRequest {
                container_job: Some(
                    fuchsia_runtime::job_default()
                        .duplicate(zx::Rights::SAME_RIGHTS)
                        .expect("Failed to dup handle"),
                ),
                wake_locks: Some(self.duplicate_lock_event()),
                ..Default::default()
            },
            zx::Instant::INFINITE,
        ) {
            Ok(Ok(res)) => {
                let wake_time = zx::BootInstant::get();
                self.update_suspend_stats(|suspend_stats| {
                    suspend_stats.success_count += 1;
                    suspend_stats.last_time_in_suspend_operations =
                        (wake_time - suspend_start_time).into();
                    suspend_stats.last_time_in_sleep =
                        zx::BootDuration::from_nanos(res.suspend_time.unwrap_or(0));
                    // The "0" here is to mimic the expected power management success string,
                    // while we don't have IRQ numbers to report.
                    suspend_stats.last_resume_reason =
                        res.resume_reason.map(|s| format!("0 {}", s));
                });
                self.lock().inspect_node.add_entry(|node| {
                    node.record_int(fobs::SUSPEND_RESUMED_AT, wake_time.into_nanos());
                });
                fuchsia_trace::instant!(
                    c"power",
                    c"suspend_container:done",
                    fuchsia_trace::Scope::Process
                );
            }
            e => {
                let wake_lock_names: Option<Vec<String>> = match e {
                    Ok(Err(frunner::SuspendError::WakeLocksExist)) => {
                        let mut names = vec![];
                        for wl in &self.active_wake_locks() {
                            names.push(wl.clone());
                        }
                        Some(names)
                    }
                    _ => None,
                };
                let wake_time = zx::BootInstant::get();
                self.update_suspend_stats(|suspend_stats| {
                    suspend_stats.fail_count += 1;
                    suspend_stats.last_failed_errno = Some(errno!(EINVAL));
                    // Power analysis tools require `Abort: ` in the case of failed suspends.
                    suspend_stats.last_resume_reason = wake_lock_names
                        .as_ref()
                        .map(|reasons| format!("Abort: {}", reasons.join(" ")));
                });
                self.lock().inspect_node.add_entry(|node| {
                    node.record_int(fobs::SUSPEND_FAILED_AT, wake_time.into_nanos());
                    if let Some(names) = wake_lock_names {
                        let names_array =
                            node.create_string_array(fobs::ACTIVE_WAKE_LOCK_NAMES, names.len());
                        for (i, name) in names.iter().enumerate() {
                            names_array.set(i, name);
                        }
                    }
                });
                fuchsia_trace::instant!(
                    c"power",
                    c"suspend_container:error",
                    fuchsia_trace::Scope::Process
                );
                return error!(EINVAL);
            }
        }

        Ok(())
    }
}

pub trait OnWakeOps: Send + Sync {
    fn on_wake(&self, current_task: &CurrentTask, baton_lease: &zx::Handle);
}

/// Creates a proxy between `remote_channel` and the returned `zx::Channel`.
///
/// The message counter's initial value will be set to 0.
///
/// The returned counter will be incremented each time there is an incoming message on the proxied
/// channel. The starnix_kernel is expected to decrement the counter when that incoming message is
/// handled.
///
/// Note that "message" in this context means channel message. This can be either a FIDL event, or
/// a response to a FIDL message from the platform.
///
/// For example, the starnix_kernel may issue a hanging get to retrieve input events. When that
/// hanging get returns, the counter will be incremented by 1. When the next hanging get has been
/// scheduled, the input subsystem decrements the counter by 1.
///
/// The proxying is done by the Starnix runner, and allows messages on the channel to wake
/// the container.
pub fn create_proxy_for_wake_events_counter_zero(
    remote_channel: zx::Channel,
    name: String,
) -> (zx::Channel, zx::Counter) {
    let (local_proxy, kernel_channel) = zx::Channel::create();
    let counter = zx::Counter::create().expect("failed to create counter");

    let local_counter =
        counter.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("Failed to duplicate counter");

    let manager = fuchsia_component::client::connect_to_protocol_sync::<frunner::ManagerMarker>()
        .expect("failed");
    manager
        .proxy_wake_channel(frunner::ManagerProxyWakeChannelRequest {
            container_job: Some(
                fuchsia_runtime::job_default()
                    .duplicate(zx::Rights::SAME_RIGHTS)
                    .expect("Failed to dup handle"),
            ),
            container_channel: Some(kernel_channel),
            remote_channel: Some(remote_channel),
            counter: Some(counter),
            name: Some(name),
            ..Default::default()
        })
        .expect("Failed to create proxy");

    (local_proxy, local_counter)
}

/// Creates a proxy between `remote_channel` and the returned `zx::Channel`.
///
/// The message counter's initial value will be set to 1, which will prevent the container from
/// suspending until the caller decrements the counter.
///
/// The returned counter will be incremented each time there is an incoming message on the proxied
/// channel. The starnix_kernel is expected to decrement the counter when that incoming message is
/// handled.
///
/// Note that "message" in this context means channel message. This can be either a FIDL event, or
/// a response to a FIDL message from the platform.
///
/// For example, the starnix_kernel may issue a hanging get to retrieve input events. When that
/// hanging get returns, the counter will be incremented by 1. When the next hanging get has been
/// scheduled, the input subsystem decrements the counter by 1.
///
/// The proxying is done by the Starnix runner, and allows messages on the channel to wake
/// the container.
pub fn create_proxy_for_wake_events_counter(
    remote_channel: zx::Channel,
    name: String,
) -> (zx::Channel, zx::Counter) {
    let (proxy, counter) = create_proxy_for_wake_events_counter_zero(remote_channel, name);

    // Increment the counter by one so that the initial incoming message to the container will
    // set the count to 0, instead of -1.
    counter.add(1).expect("Failed to add to counter");

    (proxy, counter)
}

/// Marks a message handled by decrementing `counter`.
///
/// This should be called when a proxied channel message has been handled, and the caller would
/// be ok letting the container suspend.
pub fn mark_proxy_message_handled(counter: &zx::Counter) {
    counter.add(-1).expect("Failed to decrement counter");
}

/// Marks all messages tracked by `counter` as handled.
pub fn mark_all_proxy_messages_handled(counter: &zx::Counter) {
    counter.write(0).expect("Failed to decrement counter");
}

/// Creates a watcher between clients and the Starnix runner.
///
/// Changes in the power state of the container are relayed by the event pair.
pub fn create_watcher_for_wake_events(watcher: zx::EventPair) {
    let manager = fuchsia_component::client::connect_to_protocol_sync::<frunner::ManagerMarker>()
        .expect("failed");
    manager
        .register_wake_watcher(
            frunner::ManagerRegisterWakeWatcherRequest {
                watcher: Some(watcher),
                ..Default::default()
            },
            zx::Instant::INFINITE,
        )
        .expect("Failed to register wake watcher");
}

mod test {
    #[test]
    fn test_counter_zero_initialization() {
        let (_endpoint, endpoint) = zx::Channel::create();
        let (_channel, counter) =
            super::create_proxy_for_wake_events_counter_zero(endpoint, "test".into());
        assert_eq!(counter.read(), Ok(0));
    }

    #[test]
    fn test_counter_initialization() {
        let (_endpoint, endpoint) = zx::Channel::create();
        let (_channel, counter) =
            super::create_proxy_for_wake_events_counter(endpoint, "test".into());
        assert_eq!(counter.read(), Ok(1));
    }
}
