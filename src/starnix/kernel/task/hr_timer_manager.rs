// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, AsHandleRef, HandleBased, HandleRef};
use starnix_logging::{log_debug, log_error};
use starnix_sync::{Mutex, MutexGuard};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, from_status_like_fdio};
use {fidl_fuchsia_hardware_hrtimer as fhrtimer, fuchsia_async as fasync};

use std::collections::BinaryHeap;
use std::sync::{Arc, Weak};

use crate::power::OnWakeOps;
use crate::task::{CurrentTask, HandleWaitCanceler, TargetTime, WaitCanceler};
use crate::vfs::timer::TimerOps;

const HRTIMER_DIRECTORY: &str = "/dev/class/hrtimer";
const HRTIMER_DEFAULT_ID: u64 = 6;

fn connect_to_hrtimer() -> Result<fhrtimer::DeviceSynchronousProxy, Errno> {
    let mut dir = std::fs::read_dir(HRTIMER_DIRECTORY)
        .map_err(|e| errno!(EINVAL, format!("Failed to open hrtimer directory: {e}")))?;
    let entry = dir
        .next()
        .ok_or_else(|| errno!(EINVAL, format!("No entry in the hrtimer directory")))?
        .map_err(|e| errno!(EINVAL, format!("Failed to find hrtimer device: {e}")))?;
    let path = entry
        .path()
        .into_os_string()
        .into_string()
        .map_err(|e| errno!(EINVAL, format!("Failed to parse the device entry path: {e:?}")))?;

    let (hrtimer, server_end) = fidl::endpoints::create_sync_proxy::<fhrtimer::DeviceMarker>();
    fdio::service_connect(&path, server_end.into_channel())
        .map_err(|e| errno!(EINVAL, format!("Failed to open hrtimer device: {e}")))?;

    Ok(hrtimer)
}

fn connect_to_hrtimer_async() -> Result<fhrtimer::DeviceProxy, Errno> {
    let mut dir = std::fs::read_dir(HRTIMER_DIRECTORY)
        .map_err(|e| errno!(EINVAL, format!("Failed to open hrtimer directory: {e}")))?;
    let entry = dir
        .next()
        .ok_or_else(|| errno!(EINVAL, format!("No entry in the hrtimer directory")))?
        .map_err(|e| errno!(EINVAL, format!("Failed to find hrtimer device: {e}")))?;
    let path = entry
        .path()
        .into_os_string()
        .into_string()
        .map_err(|e| errno!(EINVAL, format!("Failed to parse the device entry path: {e:?}")))?;

    let (hrtimer, server_end) = fidl::endpoints::create_proxy::<fhrtimer::DeviceMarker>()
        .map_err(|e| errno!(EINVAL, format!("Failed to create hrtimer proxy: {e}")))?;
    fdio::service_connect(&path, server_end.into_channel())
        .map_err(|e| errno!(EINVAL, format!("Failed to open hrtimer device: {e}")))?;

    Ok(hrtimer)
}

fn get_hrtimer_resolution(
    device_proxy: Option<&fhrtimer::DeviceSynchronousProxy>,
) -> Option<fhrtimer::Resolution> {
    Some(
        device_proxy?
            .get_properties(zx::MonotonicTime::INFINITE)
            .ok()?
            .timers_properties?
            .get(HRTIMER_DEFAULT_ID as usize)?
            .supported_resolutions
            .as_ref()?
            .last()?
            .clone(),
    )
}

/// The manager for high-resolution timers.
///
/// This manager is responsible for creating and managing high-resolution timers.
/// It uses a binary heap to keep track of the timers and their deadlines.
/// The timer with the soonest deadline is always at the front of the heap.
pub struct HrTimerManager {
    device_proxy: Option<fhrtimer::DeviceSynchronousProxy>,
    resolution: fhrtimer::Resolution,
    state: Mutex<HrTimerManagerState>,

    /// The event that is registered with hrtimer.
    hr_timer_event: Option<zx::Event>,
}
pub type HrTimerManagerHandle = Arc<HrTimerManager>;

#[derive(Default)]
struct HrTimerManagerState {
    /// Binary heap that stores all pending timers, with the sooner deadline having higher priority.
    timer_heap: BinaryHeap<HrTimerNode>,
    /// The deadline of the currently running timer on the `HrTimer` device.
    ///
    /// This deadline is set from the first timer in the `timer_heap`. It is used to determine when
    /// the next timer in the heap will be expired.
    ///
    /// When the `stop` method is called, the HrTimer device is stopped and the `current_deadline`
    /// is set to `None`.
    current_deadline: Option<zx::MonotonicTime>,
}

impl HrTimerManager {
    pub fn new() -> HrTimerManagerHandle {
        let device_proxy = connect_to_hrtimer().ok();
        let event = zx::Event::create();
        let hr_timer_event = event.duplicate_handle(zx::Rights::SAME_RIGHTS).ok();
        device_proxy.as_ref().map(|d| {
            d.set_event(HRTIMER_DEFAULT_ID, event, zx::MonotonicTime::INFINITE).expect("failed")
        });
        let resolution = get_hrtimer_resolution(device_proxy.as_ref())
            .unwrap_or(fhrtimer::Resolution::Duration(0));
        Arc::new(Self { device_proxy, resolution, state: Default::default(), hr_timer_event })
    }

    /// Returns the event that is signaled hrtimer when timers trigger.
    pub fn duplicate_timer_event(&self) -> Option<zx::Event> {
        self.hr_timer_event.as_ref().and_then(|e| e.duplicate_handle(zx::Rights::SAME_RIGHTS).ok())
    }

    /// Clears the `EVENT_SIGNALED` signal on the hrtimer event.
    fn reset_timer_event(&self) {
        if let Some(e) = self.hr_timer_event.as_ref() {
            e.as_handle_ref()
                .signal(zx::Signals::EVENT_SIGNALED, zx::Signals::NONE)
                .expect("Failed to clear signal on timer event");
        }
    }

    fn lock(&self) -> MutexGuard<'_, HrTimerManagerState> {
        self.state.lock()
    }

    /// Make sure the proxy to HrTimer device is active.
    fn check_connection(&self) -> Result<&fhrtimer::DeviceSynchronousProxy, Errno> {
        self.device_proxy.as_ref().ok_or(errno!(EINVAL, "No connection to HrTimer driver"))
    }

    fn resolution_nsecs(&self) -> Result<i64, Errno> {
        match self.resolution {
            fhrtimer::Resolution::Duration(r) => Ok(r),
            _ => {
                return Err(errno!(
                    EINVAL,
                    "No correct Resolution::Duration enum in the hrtimer properties"
                ));
            }
        }
    }

    #[cfg(test)]
    fn current_deadline(&self) -> Option<zx::MonotonicTime> {
        self.lock().current_deadline.clone()
    }

    /// Start the front timer in the heap.
    ///
    /// When a new timer is added to the heap, the `start_next` method is called. This method checks
    /// if the new timer has a sooner deadline than the current deadline. If it does, the HrTimer
    /// device is restarted with the new deadline. Otherwise, the current deadline remains
    /// unchanged.
    ///
    /// When a timer is removed from the heap, the `start_next` method is called again if it is the
    /// first timer in the `timer_heap`. This ensures that the next timer in the heap is started.
    fn start_next(
        self: &HrTimerManagerHandle,
        current_task: &CurrentTask,
        guard: &mut MutexGuard<'_, HrTimerManagerState>,
    ) -> Result<(), Errno> {
        let resolution_nsecs = self.resolution_nsecs()?;
        let Some(node) = guard.timer_heap.peek() else {
            return self.stop(guard);
        };

        let new_deadline = node.deadline;
        // Only restart the HrTimer device when the deadline is different from the running one.
        if guard.current_deadline == Some(new_deadline) {
            return Ok(());
        }

        let wake_source = node.wake_source.clone();
        let hrtimer_ref = node.hr_timer.clone();
        let is_interval = *hrtimer_ref.is_interval.lock();
        // Stop any currently active timers, since they no longer have the earliest deadline.
        self.stop(guard)?;
        guard.current_deadline = Some(new_deadline);

        let self_ref = self.clone();
        current_task.kernel().kthreads.spawn(move |_, current_task| {
            let mut executor = fasync::LocalExecutor::new();
            let device_proxy = connect_to_hrtimer_async().expect("failed");
            // If the deadline is in the past, set the `ticks` as 0 to trigger event right
            // away.
            let ticks = std::cmp::max(0, (new_deadline - zx::MonotonicTime::get()).into_nanos())
                / resolution_nsecs;
            let f = device_proxy.start_and_wait(
                HRTIMER_DEFAULT_ID,
                &fhrtimer::Resolution::Duration(resolution_nsecs),
                ticks as u64,
            );
            // The hrtimer client is responsible for clearing the timer fired
            // signal, so we clear it here right before starting the next
            // timer.
            self_ref.reset_timer_event();
            let r = executor.run_singlethreaded(f);
            match r {
                Ok(Ok(lease)) => {
                    let _ = hrtimer_ref
                        .event
                        .as_handle_ref()
                        .signal(zx::Signals::NONE, zx::Signals::TIMER_SIGNALED);
                    if let Some(wake_source) = wake_source.and_then(|f| f.upgrade()) {
                        let lease_channel = lease.into_channel();
                        wake_source.on_wake(current_task, &lease_channel);
                        // Drop the baton lease after wake leases in associated epfd
                        // are activated.
                        drop(lease_channel);
                    }
                    let mut guard = self_ref.lock();
                    // Remove the expired HrTimer from the heap.
                    guard.timer_heap.retain(|t| !Arc::ptr_eq(&t.hr_timer, &hrtimer_ref));

                    if guard.timer_heap.is_empty() && !is_interval {
                        // Only clear the timer event if there are no more timers to start.
                        // If there are more timers to start, we have to keep the event signaled
                        // to prevent suspension until the hanging get has been scheduled.
                        // Otherwise, we might miss a wake up.
                        self_ref.reset_timer_event();
                        return;
                    }

                    self_ref.start_next(current_task, &mut guard).expect("failed");
                }
                Ok(Err(e)) => match e {
                    fhrtimer::DriverError::Canceled => log_debug!(
                        "A new HrTimer with \
                                an earlier deadline has been started. \
                                This `StartAndWait` attempt is cancelled."
                    ),
                    _ => log_error!("HrTimer::StartAndWait driver error: {e:?}"),
                },
                Err(e) => log_error!("HrTimer::StartAndWait fidl error: {e}"),
            }
        });
        Ok(())
    }

    fn stop(
        self: &HrTimerManagerHandle,
        guard: &mut MutexGuard<'_, HrTimerManagerState>,
    ) -> Result<(), Errno> {
        guard.current_deadline = None;
        self.check_connection()?
            .stop(HRTIMER_DEFAULT_ID, zx::MonotonicTime::INFINITE)
            .map_err(|e| errno!(EINVAL, format!("HrTimer::Stop fidl error: {e}")))?
            .map_err(|e| errno!(EINVAL, format!("HrTimer::Stop driver error: {e:?}")))?;

        Ok(())
    }

    /// Add a new timer into the heap.
    pub fn add_timer(
        self: &HrTimerManagerHandle,
        current_task: &CurrentTask,
        wake_source: Option<Weak<dyn OnWakeOps>>,
        new_timer: &HrTimerHandle,
        deadline: zx::MonotonicTime,
    ) -> Result<(), Errno> {
        let mut guard = self.lock();
        let new_timer_node = HrTimerNode::new(deadline, wake_source, new_timer.clone());
        // If the deadline of a timer changes, this function will be called to update the order of
        // the `timer_heap`.
        // Check if the timer already exists and remove it to ensure the `timer_heap` remains
        // ordered by update-to-date deadline.
        guard.timer_heap.retain(|t| !Arc::ptr_eq(&t.hr_timer, new_timer));
        // Add the new timer into the heap.
        guard.timer_heap.push(new_timer_node);
        if let Some(running_timer) = guard.timer_heap.peek() {
            // If the new timer is in front, it has a sooner deadline. (Re)Start the HrTimer device
            // with the new deadline.
            if Arc::ptr_eq(&running_timer.hr_timer, new_timer) {
                return self.start_next(current_task, &mut guard);
            }
        }
        Ok(())
    }

    /// Remove a timer from the heap.
    pub fn remove_timer(
        self: &HrTimerManagerHandle,
        current_task: &CurrentTask,
        timer: &HrTimerHandle,
    ) -> Result<(), Errno> {
        let mut guard = self.lock();
        if let Some(running_timer_node) = guard.timer_heap.peek() {
            if Arc::ptr_eq(&running_timer_node.hr_timer, timer) {
                guard.timer_heap.pop();
                self.start_next(current_task, &mut guard)?;
                return Ok(());
            }
        }

        // Find the timer to stop and remove
        guard.timer_heap.retain(|tn| !Arc::ptr_eq(&tn.hr_timer, timer));

        Ok(())
    }
}

#[derive(Debug)]
pub struct HrTimer {
    event: Arc<zx::Event>,

    /// True iff the timer is currently set to trigger at an interval.
    ///
    /// This is used to determine at which point the hrtimer event (not
    /// `HrTimer::event` but the one that is shared with the actual driver)
    /// should be cleared.
    ///
    /// If this is true, the timer manager will wait to clear the timer event
    /// until the next timer request has been sent to the driver. This prevents
    /// lost wake ups where the container happens to suspend between two instances
    /// of an interval timer triggering.
    pub is_interval: Mutex<bool>,
}
pub type HrTimerHandle = Arc<HrTimer>;

impl HrTimer {
    pub fn new() -> HrTimerHandle {
        Arc::new(Self { event: Arc::new(zx::Event::create()), is_interval: Mutex::new(false) })
    }

    pub fn event(&self) -> zx::Event {
        self.event
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .expect("Duplicate hrtimer event handle")
    }
}

impl TimerOps for HrTimerHandle {
    fn start(
        &self,
        current_task: &CurrentTask,
        source: Option<Weak<dyn OnWakeOps>>,
        deadline: TargetTime,
    ) -> Result<(), Errno> {
        // Before (re)starting the timer, ensure the signal is cleared.
        self.event
            .as_handle_ref()
            .signal(zx::Signals::TIMER_SIGNALED, zx::Signals::NONE)
            .map_err(|status| from_status_like_fdio!(status))?;
        current_task.kernel().hrtimer_manager.add_timer(
            current_task,
            source,
            self,
            deadline.estimate_monotonic(),
        )?;
        Ok(())
    }

    fn stop(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        // Clear the signal when stopping the hrtimer.
        self.event
            .as_handle_ref()
            .signal(zx::Signals::TIMER_SIGNALED, zx::Signals::NONE)
            .map_err(|status| from_status_like_fdio!(status))?;
        Ok(current_task.kernel().hrtimer_manager.remove_timer(current_task, self)?)
    }

    fn wait_canceler(&self, canceler: HandleWaitCanceler) -> WaitCanceler {
        WaitCanceler::new_event(Arc::downgrade(&self.event), canceler)
    }

    fn as_handle_ref(&self) -> HandleRef<'_> {
        self.event.as_handle_ref()
    }
}

/// Represents a node of `HrTimer` in the binary heap used by the `HrTimerManager`.
struct HrTimerNode {
    /// The deadline of the associated `HrTimer`.
    ///
    /// This is used to determine the order of the nodes in the heap.
    deadline: zx::MonotonicTime,

    /// The source where initiated this `HrTimer`.
    ///
    /// When the timer expires, the system will be woken up if necessary. The `on_wake` callback
    /// will be triggered with a baton lease to prevent further suspend while Starnix handling the
    /// wake event.
    wake_source: Option<Weak<dyn OnWakeOps>>,

    hr_timer: HrTimerHandle,
}

impl HrTimerNode {
    fn new(
        deadline: zx::MonotonicTime,
        wake_source: Option<Weak<dyn OnWakeOps>>,
        hr_timer: HrTimerHandle,
    ) -> Self {
        Self { deadline, wake_source, hr_timer }
    }
}

impl PartialEq for HrTimerNode {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline
            && Arc::ptr_eq(&self.hr_timer, &other.hr_timer)
            && match (self.wake_source.as_ref(), other.wake_source.as_ref()) {
                (Some(this), Some(other)) => Weak::ptr_eq(this, other),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for HrTimerNode {}

impl Ord for HrTimerNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Sooner the deadline, higher the priority.
        match other.deadline.cmp(&self.deadline) {
            std::cmp::Ordering::Equal => {
                Arc::as_ptr(&other.hr_timer).cmp(&Arc::as_ptr(&self.hr_timer))
            }
            other => other,
        }
    }
}

impl PartialOrd for HrTimerNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use {fidl_fuchsia_hardware_hrtimer as fhrtimer, fuchsia_async as fasync};

    use crate::testing::create_kernel_and_task;

    use super::*;

    /// Returns a mocked HrTimer::Device client sync proxy and its server running in a spawned
    /// thread.
    ///
    /// Note: fuchsia::test with async starts a fuchsia-async executor, which the mock server needs.
    /// Having a sync fidl call running on an async executor that is talking to something that
    /// needs to run on the same executor is troublesome. Mutithread should be set in these tests.
    /// It should never happen outside of test code.
    fn mock_hrtimer_connection() -> fhrtimer::DeviceSynchronousProxy {
        let (hrtimer, mut stream) =
            fidl::endpoints::create_sync_proxy_and_stream::<fhrtimer::DeviceMarker>().unwrap();
        let fut = async move {
            while let Some(Ok(event)) = stream.next().await {
                match event {
                    fhrtimer::DeviceRequest::Start { responder, .. } => {
                        responder.send(Ok(())).expect("");
                    }
                    fhrtimer::DeviceRequest::Stop { responder, .. } => {
                        responder.send(Ok(())).expect("");
                    }
                    fhrtimer::DeviceRequest::GetTicksLeft { responder, .. } => {
                        responder.send(Ok(1)).expect("");
                    }
                    fhrtimer::DeviceRequest::SetEvent { responder, .. } => {
                        responder.send(Ok(())).expect("");
                    }
                    fhrtimer::DeviceRequest::StartAndWait { responder, .. } => {
                        responder.send(Err(fhrtimer::DriverError::InternalError)).expect("");
                    }
                    fhrtimer::DeviceRequest::GetProperties { responder, .. } => {
                        responder.send(fhrtimer::Properties { ..Default::default() }).expect("");
                    }
                    fhrtimer::DeviceRequest::_UnknownMethod { .. } => todo!(),
                }
            }
        };

        fasync::Task::spawn(fut).detach();

        hrtimer
    }

    fn init_hr_timer_manager() -> HrTimerManagerHandle {
        let proxy = mock_hrtimer_connection();
        Arc::new(HrTimerManager {
            device_proxy: Some(proxy),
            resolution: fhrtimer::Resolution::Duration(10000),
            state: Default::default(),
            hr_timer_event: None,
        })
    }

    #[fuchsia::test(threads = 2)]
    async fn hr_timer_manager_add_timers() {
        let hrtimer_manager = init_hr_timer_manager();
        let (_, current_task) = create_kernel_and_task();
        let soonest_deadline = zx::MonotonicTime::from_nanos(1);
        let timer1 = HrTimer::new();
        let timer2 = HrTimer::new();
        let timer3 = HrTimer::new();

        // Add three timers into the heap.
        assert!(hrtimer_manager
            .add_timer(&current_task, None, &timer3, zx::MonotonicTime::from_nanos(3))
            .is_ok());
        assert!(hrtimer_manager
            .add_timer(&current_task, None, &timer2, zx::MonotonicTime::from_nanos(2))
            .is_ok());
        assert!(hrtimer_manager.add_timer(&current_task, None, &timer1, soonest_deadline).is_ok());

        // Make sure the deadline of the current running timer is the soonest.
        assert!(hrtimer_manager.current_deadline().is_some_and(|d| d == soonest_deadline));
    }

    #[fuchsia::test(threads = 2)]
    async fn hr_timer_manager_add_duplicate_timers() {
        let hrtimer_manager = init_hr_timer_manager();
        let (_, current_task) = create_kernel_and_task();

        let timer1 = HrTimer::new();
        let sooner_deadline = zx::MonotonicTime::after(zx::Duration::from_seconds(1));
        assert!(hrtimer_manager.add_timer(&current_task, None, &timer1, sooner_deadline).is_ok());
        assert!(hrtimer_manager.current_deadline().is_some_and(|d| d == sooner_deadline));

        let later_deadline = zx::MonotonicTime::after(zx::Duration::from_seconds(1));
        assert!(later_deadline > sooner_deadline);
        assert!(hrtimer_manager.add_timer(&current_task, None, &timer1, later_deadline).is_ok());
        assert!(hrtimer_manager.current_deadline().is_some_and(|d| d == later_deadline));
        // Make sure no duplicate timers.
        assert_eq!(hrtimer_manager.lock().timer_heap.len(), 1);
    }

    #[fuchsia::test(threads = 2)]
    async fn hr_timer_manager_remove_timers() {
        let hrtimer_manager = init_hr_timer_manager();
        let (_, current_task) = create_kernel_and_task();

        let timer1 = HrTimer::new();
        let timer2 = HrTimer::new();
        let timer2_deadline = zx::MonotonicTime::after(zx::Duration::from_seconds(2));
        let timer3 = HrTimer::new();
        let timer3_deadline = zx::MonotonicTime::after(zx::Duration::from_seconds(3));

        assert!(hrtimer_manager.add_timer(&current_task, None, &timer3, timer3_deadline).is_ok());
        assert!(hrtimer_manager.add_timer(&current_task, None, &timer2, timer2_deadline).is_ok());
        assert!(hrtimer_manager
            .add_timer(
                &current_task,
                None,
                &timer1,
                zx::MonotonicTime::after(zx::Duration::from_seconds(1))
            )
            .is_ok());

        assert!(hrtimer_manager.remove_timer(&current_task, &timer1).is_ok());
        assert!(hrtimer_manager.current_deadline().is_some_and(|d| d == timer2_deadline));

        assert!(hrtimer_manager.remove_timer(&current_task, &timer2).is_ok());
        assert!(hrtimer_manager.current_deadline().is_some_and(|d| d == timer3_deadline));
    }

    #[fuchsia::test(threads = 2)]
    async fn hr_timer_manager_clear_heap() {
        let hrtimer_manager = init_hr_timer_manager();
        let (_, current_task) = create_kernel_and_task();
        let timer = HrTimer::new();
        assert!(hrtimer_manager
            .add_timer(
                &current_task,
                None,
                &timer,
                zx::MonotonicTime::after(zx::Duration::from_seconds(1))
            )
            .is_ok());
        assert!(hrtimer_manager.remove_timer(&current_task, &timer).is_ok());
        assert!(hrtimer_manager.current_deadline().is_none());
    }

    #[fuchsia::test(threads = 2)]
    async fn hr_timer_manager_update_deadline() {
        let hrtimer_manager = init_hr_timer_manager();
        let (_, current_task) = create_kernel_and_task();

        let timer = HrTimer::new();
        let sooner_deadline = zx::MonotonicTime::after(zx::Duration::from_seconds(1));
        let later_deadline = zx::MonotonicTime::after(zx::Duration::from_seconds(2));

        assert!(hrtimer_manager.add_timer(&current_task, None, &timer, later_deadline).is_ok());
        assert!(hrtimer_manager.current_deadline().is_some_and(|d| d == later_deadline));
        assert!(hrtimer_manager.add_timer(&current_task, None, &timer, sooner_deadline).is_ok());
        // Make sure no duplicate timers.
        assert_eq!(hrtimer_manager.lock().timer_heap.len(), 1);
        assert!(hrtimer_manager.current_deadline().is_some_and(|d| d == sooner_deadline));
    }

    #[fuchsia::test]
    async fn hr_timer_node_cmp() {
        let time = zx::MonotonicTime::after(zx::Duration::from_seconds(1));
        let timer1 = HrTimer::new();
        let node1 = HrTimerNode::new(time, None, timer1.clone());
        let timer2 = HrTimer::new();
        let node2 = HrTimerNode::new(time, None, timer2.clone());

        assert!(node1 != node2 && node1.cmp(&node2) != std::cmp::Ordering::Equal);
    }
}
