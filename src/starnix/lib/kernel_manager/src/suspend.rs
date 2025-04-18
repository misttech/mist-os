// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Kernels;
use anyhow::Error;
use fidl::{HandleBased, Peered};
use fidl_fuchsia_starnix_runner as fstarnixrunner;
use fuchsia_sync::Mutex;
use log::warn;
use std::sync::Arc;
use zx::{AsHandleRef, Task};

/// The signal that the kernel raises to indicate that it's awake.
pub const AWAKE_SIGNAL: zx::Signals = zx::Signals::USER_0;

/// The signal that the kernel raises to indicate that it's suspended.
pub const ASLEEP_SIGNAL: zx::Signals = zx::Signals::USER_1;

pub struct WakeSource {
    counter: zx::Counter,
    name: String,
}

impl WakeSource {
    pub fn new(counter: zx::Counter, name: String) -> Self {
        Self { counter, name }
    }

    fn as_wait_item(&self) -> zx::WaitItem<'_> {
        zx::WaitItem {
            handle: self.counter.as_handle_ref(),
            waitfor: zx::Signals::COUNTER_POSITIVE,
            pending: zx::Signals::empty(),
        }
    }
}

pub type WakeSources = std::collections::HashMap<zx::Koid, WakeSource>;

#[derive(Default)]
pub struct SuspendContext {
    pub wake_sources: Arc<Mutex<WakeSources>>,
    pub wake_watchers: Arc<Mutex<Vec<zx::EventPair>>>,
}

/// Suspends the container specified by the `payload`.
pub async fn suspend_container(
    payload: fstarnixrunner::ManagerSuspendContainerRequest,
    suspend_context: &Arc<SuspendContext>,
    kernels: &Kernels,
) -> Result<
    Result<fstarnixrunner::ManagerSuspendContainerResponse, fstarnixrunner::SuspendError>,
    Error,
> {
    fuchsia_trace::duration!(c"power", c"starnix-runner:suspending-container");
    let Some(container_job) = payload.container_job else {
        warn!(
            "error suspending container: could not find container job {:?}",
            payload.container_job
        );
        return Ok(Err(fstarnixrunner::SuspendError::SuspendFailure));
    };

    // These handles need to kept alive until the end of the block, as they will
    // resume the kernel when dropped.
    log::info!("Suspending all container processes.");
    let _suspend_handles = match suspend_job(&container_job).await {
        Ok(handles) => handles,
        Err(e) => {
            warn!("error suspending container {:?}", e);
            fuchsia_trace::instant!(
                c"power",
                c"starnix-runner:suspend-failed-actual",
                fuchsia_trace::Scope::Process
            );
            return Ok(Err(fstarnixrunner::SuspendError::SuspendFailure));
        }
    };
    log::info!("Finished suspending all container processes.");

    let suspend_start = zx::BootInstant::get();

    if let Some(wake_locks) = payload.wake_locks {
        match wake_locks
            .wait_handle(zx::Signals::EVENT_SIGNALED, zx::MonotonicInstant::ZERO)
            .to_result()
        {
            Ok(_) => {
                // There were wake locks active after suspending all processes, resume
                // and fail the suspend call.
                warn!("error suspending container: Linux wake locks exist");
                fuchsia_trace::instant!(
                    c"power",
                    c"starnix-runner:suspend-failed-with-wake-locks",
                    fuchsia_trace::Scope::Process
                );
                return Ok(Err(fstarnixrunner::SuspendError::WakeLocksExist));
            }
            Err(_) => {}
        };
    }

    {
        log::info!("Notifying wake watchers of container suspend.");
        let watchers = suspend_context.wake_watchers.lock();
        for event in watchers.iter() {
            let (clear_mask, set_mask) = (AWAKE_SIGNAL, ASLEEP_SIGNAL);
            event.signal_peer(clear_mask, set_mask)?;
        }
    }
    kernels.drop_wake_lease(&container_job)?;

    let wake_sources = suspend_context.wake_sources.lock();
    let mut wait_items: Vec<zx::WaitItem<'_>> =
        wake_sources.iter().map(|(_, w)| w.as_wait_item()).collect();

    // TODO: We will likely have to handle a larger number of wake sources in the
    // future, at which point we may want to consider a Port-based approach. This
    // would also allow us to unblock this thread.
    {
        fuchsia_trace::duration!(c"power", c"starnix-runner:waiting-on-container-wake");
        if wait_items.len() > 0 {
            log::info!("Waiting on container to receive incoming message on wake proxies");
            match zx::object_wait_many(&mut wait_items, zx::MonotonicInstant::INFINITE) {
                Ok(_) => (),
                Err(e) => {
                    warn!("error waiting for wake event {:?}", e);
                }
            };
        }
    }
    log::info!("Finished waiting on container wake proxies.");

    let mut resume_reason: Option<String> = None;
    for wait_item in &wait_items {
        if wait_item.pending.contains(zx::Signals::COUNTER_POSITIVE) {
            let koid = wait_item.handle.get_koid().unwrap();
            if let Some(event) = wake_sources.get(&koid) {
                log::info!(
                    "Woke container from sleep for: {}, count: {:?}",
                    event.name,
                    event.counter.read()
                );
                resume_reason = Some(event.name.clone());
            }
        }
    }

    kernels.acquire_wake_lease(&container_job).await?;

    log::info!("Notifying wake watchers of container wakeup.");
    let watchers = suspend_context.wake_watchers.lock();
    for event in watchers.iter() {
        let (clear_mask, set_mask) = (ASLEEP_SIGNAL, AWAKE_SIGNAL);
        event.signal_peer(clear_mask, set_mask)?;
    }

    log::info!("Returning successfully from suspend container");
    Ok(Ok(fstarnixrunner::ManagerSuspendContainerResponse {
        suspend_time: Some((zx::BootInstant::get() - suspend_start).into_nanos()),
        resume_reason,
        ..Default::default()
    }))
}

/// Suspends the provided `zx::Job` by suspending each process in the job individually.
///
/// Returns the suspend handles for all the suspended processes.
///
/// Returns an error if any individual suspend failed. Any suspend handles will be dropped before
/// the error is returned.
async fn suspend_job(kernel_job: &zx::Job) -> Result<Vec<zx::Handle>, Error> {
    let mut handles = std::collections::HashMap::<zx::Koid, zx::Handle>::new();
    loop {
        let process_koids = kernel_job.processes().expect("failed to get processes");
        let mut found_new_process = false;
        let mut processes = vec![];

        for process_koid in process_koids {
            if handles.get(&process_koid).is_some() {
                continue;
            }

            found_new_process = true;

            if let Ok(process_handle) = kernel_job.get_child(&process_koid, zx::Rights::SAME_RIGHTS)
            {
                let process = zx::Process::from_handle(process_handle);
                match process.suspend() {
                    Ok(suspend_handle) => {
                        handles.insert(process_koid, suspend_handle);
                    }
                    Err(zx::Status::BAD_STATE) => {
                        // The process was already dead or dying, and thus can't be suspended.
                        continue;
                    }
                    Err(e) => {
                        log::warn!("Failed process suspension: {:?}", e);
                        return Err(e.into());
                    }
                };
                processes.push(process);
            }
        }

        for process in processes {
            let threads = process.threads().expect("failed to get threads");
            for thread_koid in &threads {
                fuchsia_trace::duration!(c"power", c"starnix-runner:suspend_kernel", "thread_koid" => *thread_koid);
                if let Ok(thread) = process.get_child(&thread_koid, zx::Rights::SAME_RIGHTS) {
                    match thread
                        .wait_handle(
                            zx::Signals::THREAD_SUSPENDED,
                            zx::MonotonicInstant::after(zx::MonotonicDuration::INFINITE),
                        )
                        .to_result()
                    {
                        Err(e) => {
                            log::warn!("Error waiting for task suspension: {:?}", e);
                            return Err(e.into());
                        }
                        _ => {}
                    }
                }
            }
        }

        if !found_new_process {
            break;
        }
    }

    Ok(handles.into_values().collect())
}
