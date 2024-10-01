// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod kernels;

use anyhow::{anyhow, Error};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy, ServerEnd};
use fidl::HandleBased;
use fuchsia_component::client as fclient;
use fuchsia_sync::Mutex;
use fuchsia_zircon::{AsHandleRef, Task};
use futures::TryStreamExt;
use kernels::Kernels;
use rand::Rng;
use std::future::Future;
use std::mem::MaybeUninit;
use std::sync::Arc;
use tracing::{debug, warn};
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_io as fio,
    fidl_fuchsia_starnix_container as fstarnix, fidl_fuchsia_starnix_runner as fstarnixrunner,
    fuchsia_zircon as zx,
};

/// The name of the collection that the starnix_kernel is run in.
const KERNEL_COLLECTION: &str = "kernels";

/// The name of the protocol the kernel exposes for running containers.
///
/// This protocol is actually fuchsia.component.runner.ComponentRunner. We
/// expose the implementation using this name to avoid confusion with copy
/// of the fuchsia.component.runner.ComponentRunner protocol used for
/// running component inside the container.
const CONTAINER_RUNNER_PROTOCOL: &str = "fuchsia.starnix.container.Runner";

/// The signal that the runner raises when handing over an event to the kernel.
const RUNNER_SIGNAL: zx::Signals = zx::Signals::USER_0;

/// The signal that the kernel raises to indicate that a message has been handled.
const KERNEL_SIGNAL: zx::Signals = zx::Signals::USER_1;

#[allow(dead_code)]
pub struct StarnixKernel {
    /// The name of the kernel intsance in the kernels collection.
    name: String,

    /// The controller used to control the kernel component's lifecycle.
    ///
    /// The kernel runs in a "kernels" collection within that realm.
    controller_proxy: fcomponent::ControllerProxy,

    /// The directory exposed by the Starnix kernel.
    ///
    /// This directory can be used to connect to services offered by the kernel.
    exposed_dir: fio::DirectoryProxy,

    /// An opaque token representing the container component.
    component_instance: zx::Event,

    /// The job the kernel lives under.
    job: Arc<zx::Job>,

    /// The currently active wake lease for the container.
    wake_lease: Mutex<Option<zx::EventPair>>,
}

impl StarnixKernel {
    /// Creates a new instance of `starnix_kernel`.
    ///
    /// This is done by creating a new child in the `kernels` collection.
    ///
    /// Returns the kernel and a future that will resolve when the kernel has stopped.
    pub async fn create(
        realm: fcomponent::RealmProxy,
        kernel_url: &str,
        start_info: frunner::ComponentStartInfo,
        controller: ServerEnd<frunner::ComponentControllerMarker>,
    ) -> Result<(Self, impl Future<Output = ()>), Error> {
        let kernel_name = generate_kernel_name(&start_info)?;
        let component_instance = start_info
            .component_instance
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!("expected to find component_instance in ComponentStartInfo")
            })?
            .duplicate_handle(zx::Rights::SAME_RIGHTS)?;

        // Create the `starnix_kernel`.
        let (controller_proxy, controller_server_end) = fidl::endpoints::create_proxy()?;
        realm
            .create_child(
                &fdecl::CollectionRef { name: KERNEL_COLLECTION.into() },
                &fdecl::Child {
                    name: Some(kernel_name.clone()),
                    url: Some(kernel_url.to_string()),
                    startup: Some(fdecl::StartupMode::Lazy),
                    ..Default::default()
                },
                fcomponent::CreateChildArgs {
                    controller: Some(controller_server_end),
                    ..Default::default()
                },
            )
            .await?
            .map_err(|e| anyhow::anyhow!("failed to create kernel: {:?}", e))?;

        // Start the kernel to obtain an `ExecutionController`.
        let (execution_controller_proxy, execution_controller_server_end) =
            fidl::endpoints::create_proxy()?;
        controller_proxy
            .start(fcomponent::StartChildArgs::default(), execution_controller_server_end)
            .await?
            .map_err(|e| anyhow::anyhow!("failed to start kernel: {:?}", e))?;

        let exposed_dir = open_exposed_directory(&realm, &kernel_name, KERNEL_COLLECTION).await?;
        let container_runner = fclient::connect_to_named_protocol_at_dir_root::<
            frunner::ComponentRunnerMarker,
        >(&exposed_dir, CONTAINER_RUNNER_PROTOCOL)?;

        // Actually start the container.
        container_runner.start(start_info, controller)?;

        // Ask the kernel for its job.
        let container_controller =
            fclient::connect_to_protocol_at_dir_root::<fstarnix::ControllerMarker>(&exposed_dir)?;
        let fstarnix::ControllerGetJobHandleResponse { job, .. } =
            container_controller.get_job_handle().await?;
        let Some(job) = job else {
            anyhow::bail!("expected to find job in ControllerGetJobHandleResponse");
        };

        let kernel = Self {
            name: kernel_name,
            controller_proxy,
            exposed_dir,
            component_instance,
            job: Arc::new(job),
            wake_lease: Default::default(),
        };
        let on_stop = async move {
            _ = execution_controller_proxy.into_channel().unwrap().on_closed().await;
        };
        Ok((kernel, on_stop))
    }

    /// Gets the opaque token representing the container component.
    pub fn component_instance(&self) -> &zx::Event {
        &self.component_instance
    }

    /// Gets the job the kernel lives under.
    pub fn job(&self) -> &Arc<zx::Job> {
        &self.job
    }

    /// Connect to the specified protocol exposed by the kernel.
    pub fn connect_to_protocol<P: DiscoverableProtocolMarker>(&self) -> Result<P::Proxy, Error> {
        fclient::connect_to_protocol_at_dir_root::<P>(&self.exposed_dir)
    }

    /// Destroys the Starnix kernel that is running the given test.
    pub async fn destroy(self) -> Result<(), Error> {
        self.controller_proxy
            .destroy()
            .await?
            .map_err(|e| anyhow!("kernel component destruction failed: {e:?}"))?;
        let mut event_stream = self.controller_proxy.take_event_stream();
        loop {
            match event_stream.try_next().await {
                Ok(Some(_)) => continue,
                Ok(None) => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[derive(Default)]
pub struct SuspendContext {
    suspended_processes: Arc<Mutex<Vec<zx::Handle>>>,
    resume_events: Arc<Mutex<Vec<zx::EventPair>>>,
}

/// Generate a random name for the kernel.
///
/// We used to include some human-readable parts in the name, but people were
/// tempted to make them load-bearing. We now just generate 7 random alphanumeric
/// characters.
fn generate_kernel_name(_start_info: &frunner::ComponentStartInfo) -> Result<String, Error> {
    let random_id: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();
    Ok(random_id)
}

async fn open_exposed_directory(
    realm: &fcomponent::RealmProxy,
    child_name: &str,
    collection_name: &str,
) -> Result<fio::DirectoryProxy, Error> {
    let (directory_proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;
    realm
        .open_exposed_dir(
            &fdecl::ChildRef { name: child_name.into(), collection: Some(collection_name.into()) },
            server_end,
        )
        .await?
        .map_err(|e| {
            anyhow!(
                "failed to bind to child {} in collection {:?}: {:?}",
                child_name,
                collection_name,
                e
            )
        })?;
    Ok(directory_proxy)
}

pub async fn serve_starnix_manager(
    mut stream: fstarnixrunner::ManagerRequestStream,
    suspend_context: Arc<SuspendContext>,
    kernels: &Kernels,
) -> Result<(), Error> {
    while let Some(event) = stream.try_next().await? {
        match event {
            fstarnixrunner::ManagerRequest::Suspend { responder, .. } => {
                suspend_kernels(kernels, &suspend_context.suspended_processes).await;
                if let Err(e) = responder.send() {
                    warn!("error replying to suspend request: {e}");
                }
            }
            fstarnixrunner::ManagerRequest::SuspendContainer { payload, responder, .. } => {
                let Some(container_job) = payload.container_job else {
                    if let Err(e) =
                        responder.send(Err(fstarnixrunner::SuspendError::SuspendFailure))
                    {
                        warn!("error responding to suspend request {:?}", e);
                    }
                    continue;
                };

                // These handles need to kept alive until the end of the block, as they will
                // resume the kernel when dropped.
                let _suspend_handles = match suspend_kernel(&container_job).await {
                    Ok(handles) => handles,
                    Err(e) => {
                        warn!("error suspending container {:?}", e);
                        if let Err(e) =
                            responder.send(Err(fstarnixrunner::SuspendError::SuspendFailure))
                        {
                            warn!("error responding to suspend request {:?}", e);
                        }
                        continue;
                    }
                };

                let suspend_start = zx::BootInstant::get();

                if let Some(wake_locks) = payload.wake_locks {
                    match wake_locks
                        .wait_handle(zx::Signals::EVENT_SIGNALED, zx::MonotonicInstant::ZERO)
                    {
                        Ok(_) => {
                            // There were wake locks active after suspending all processes, resume
                            // and fail the suspend call.
                            if let Err(e) =
                                responder.send(Err(fstarnixrunner::SuspendError::WakeLocksExist))
                            {
                                warn!("error responding to suspend request {:?}", e);
                            }
                            continue;
                        }
                        Err(_) => {}
                    };
                }

                kernels.drop_wake_lease(&container_job)?;

                let resume_events = suspend_context.resume_events.lock();
                let mut wait_items: Vec<zx::WaitItem<'_>> = resume_events
                    .iter()
                    .map(|e: &zx::EventPair| zx::WaitItem {
                        handle: e.as_handle_ref(),
                        waitfor: RUNNER_SIGNAL,
                        pending: zx::Signals::empty(),
                    })
                    .collect();

                // TODO: We will likely have to handle a larger number of wake sources in the
                // future, at which point we may want to consider a Port-based approach. This
                // would also allow us to unblock this thread.
                match zx::object_wait_many(&mut wait_items, zx::MonotonicInstant::INFINITE) {
                    Ok(_) => {}
                    Err(e) => {
                        warn!("error waiting for wake event {:?}", e);
                    }
                };

                kernels.acquire_wake_lease(&container_job).await?;

                let response = fstarnixrunner::ManagerSuspendContainerResponse {
                    suspend_time: Some((zx::BootInstant::get() - suspend_start).into_nanos()),
                    ..Default::default()
                };
                if let Err(e) = responder.send(Ok(&response)) {
                    warn!("error responding to suspend request {:?}", e);
                }
            }
            fstarnixrunner::ManagerRequest::ProxyWakeChannel { payload, .. } => {
                let fstarnixrunner::ManagerProxyWakeChannelRequest {
                    // TODO: Handle more than one container.
                    container_job: Some(_container_job),
                    remote_channel: Some(remote_channel),
                    container_channel: Some(container_channel),
                    resume_event: Some(resume_event),
                    ..
                } = payload
                else {
                    continue;
                };

                let proxy = ChannelProxy { container_channel, remote_channel, resume_event };
                suspend_context.resume_events.lock().push(
                    proxy.resume_event.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("failed"),
                );

                start_proxy(proxy, suspend_context.resume_events.clone());
            }
            fstarnixrunner::ManagerRequest::Resume { .. } => {
                resume_kernels(&suspend_context.suspended_processes)
            }
            _ => {}
        }
    }
    Ok(())
}

struct ChannelProxy {
    /// The channel that is connected to the container component.
    container_channel: zx::Channel,

    /// The channel that is connected to a peer outside of the container component.
    remote_channel: zx::Channel,

    /// The resume event that is signaled when messages are proxied into the container.
    resume_event: zx::EventPair,
}

/// Starts a thread that proxies messages between `proxy.container_channel` and
/// `proxy.remote_channel`. The thread will exit when either of the channels' peer is closed.
///
/// When the proxy exits, `proxy.resume_event` will be removed from `resume_events`.
fn start_proxy(proxy: ChannelProxy, resume_events: Arc<Mutex<Vec<zx::EventPair>>>) {
    // TODO: We will likely have to handle a larger number of wake sources in the future,
    // at which point we may want to consider a Port-based approach, and reduce the number
    // of threads.
    std::thread::spawn(move || {
        let mut bounce_bytes = [MaybeUninit::uninit(); zx::sys::ZX_CHANNEL_MAX_MSG_BYTES as usize];
        let mut bounce_handles =
            [const { MaybeUninit::uninit() }; zx::sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize];

        'outer: loop {
            const CONTAINER_CHANNEL_INDEX: usize = 0;
            const REMOTE_CHANNEL_INDEX: usize = 1;

            // Wait on messages from both the container and remote channel endpoints.
            let mut wait_items = [
                zx::WaitItem {
                    handle: proxy.container_channel.as_handle_ref(),
                    waitfor: zx::Signals::CHANNEL_READABLE | zx::Signals::CHANNEL_PEER_CLOSED,
                    pending: zx::Signals::empty(),
                },
                zx::WaitItem {
                    handle: proxy.remote_channel.as_handle_ref(),
                    waitfor: zx::Signals::CHANNEL_READABLE | zx::Signals::CHANNEL_PEER_CLOSED,
                    pending: zx::Signals::empty(),
                },
            ];

            match zx::object_wait_many(&mut wait_items, zx::MonotonicInstant::INFINITE) {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Failed to wait on proxied channels in runner: {:?}", e);
                    break 'outer;
                }
            };

            // Forward messages in both directions. Only messages that are entering the container
            // should signal `proxy.resume_event`, since those are the only messages that should
            // wake the container if it's suspended.
            if forward_message(
                &wait_items[CONTAINER_CHANNEL_INDEX],
                &proxy.container_channel,
                &proxy.remote_channel,
                None,
                &mut bounce_bytes,
                &mut bounce_handles,
            )
            .is_err()
            {
                tracing::warn!("Proxy failed to forward message from kernel");
                break 'outer;
            }
            if forward_message(
                &wait_items[REMOTE_CHANNEL_INDEX],
                &proxy.remote_channel,
                &proxy.container_channel,
                Some(&proxy.resume_event),
                &mut bounce_bytes,
                &mut bounce_handles,
            )
            .is_err()
            {
                tracing::warn!("Proxy failed to forward message to kernel");
                break 'outer;
            }
        }

        resume_events.lock().retain(|e| e.get_koid() != proxy.resume_event.get_koid());
    });
}

/// Forwards any pending messages on `read_channel` to `write_channel`, if the `wait_item.pending`
/// contains `CHANNEL_READABLE`.
///
/// If `event` is `Some`, it will be signaled with `EVENT_SIGNALED` if a message was read and
/// written.
fn forward_message(
    wait_item: &zx::WaitItem<'_>,
    read_channel: &zx::Channel,
    write_channel: &zx::Channel,
    event: Option<&zx::EventPair>,
    bytes: &mut [MaybeUninit<u8>; zx::sys::ZX_CHANNEL_MAX_MSG_BYTES as usize],
    handles: &mut [MaybeUninit<zx::Handle>; zx::sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize],
) -> Result<(), Error> {
    if wait_item.pending.contains(zx::Signals::CHANNEL_READABLE) {
        let (actual_bytes, actual_handles) = match read_channel.read_uninit(bytes, handles) {
            zx::ChannelReadResult::Ok(r) => r,
            _ => return Err(anyhow!("Failed to read from channel")),
        };
        write_channel.write(actual_bytes, actual_handles)?;

        if let Some(event) = event {
            // Signal event with `RUNNER_SIGNAL`, indicating that an event is being sent to
            // the kernel.
            let (clear_mask, set_mask) = (KERNEL_SIGNAL, RUNNER_SIGNAL);
            event.signal_handle(clear_mask, set_mask)?;

            // Wait for the kernel endpoint to signal that the event has been handled, and
            // that it is now safe to suspend the container again.
            match event.wait_handle(KERNEL_SIGNAL, zx::MonotonicInstant::INFINITE) {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Failed to wait on proxied channels in runner: {:?}", e);
                    return Err(anyhow!("Failed to wait on signal from kernel"));
                }
            };

            // Clear any remaining signals from the event before continuing the proxy.
            let (clear_mask, set_mask) = (KERNEL_SIGNAL | RUNNER_SIGNAL, zx::Signals::NONE);
            event.signal_handle(clear_mask, set_mask)?;
        }
    }
    Ok(())
}

async fn suspend_kernels(kernels: &Kernels, suspended_processes: &Mutex<Vec<zx::Handle>>) {
    fuchsia_trace::duration!(c"starnix_runner", c"suspend_kernels");

    debug!("suspending processes...");
    for job in kernels.all_jobs() {
        suspended_processes
            .lock()
            .append(&mut suspend_kernel(&job).await.expect("Failed to suspend kernel job"));
    }
    debug!("...done suspending processes");
}

fn resume_kernels(suspended_processes: &Mutex<Vec<zx::Handle>>) {
    debug!("requesting resume...");
    // Drop all the suspend handles to resume the kernel.
    *suspended_processes.lock() = vec![];
    debug!("...requested resume (completes asynchronously)");
}

/// Suspends `kernel` by suspending all the processes in the kernel's job.
async fn suspend_kernel(kernel_job: &zx::Job) -> Result<Vec<zx::Handle>, Error> {
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

            if let Ok(process_handle) =
                kernel_job.get_child(&process_koid, zx::Rights::SAME_RIGHTS.bits())
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
                        tracing::warn!("Failed process suspension: {:?}", e);
                        return Err(e.into());
                    }
                };
                processes.push(process);
            }
        }

        for process in processes {
            let threads = process.threads().expect("failed to get threads");
            for thread_koid in &threads {
                if let Ok(thread_handle) =
                    process.get_child(&thread_koid, zx::Rights::SAME_RIGHTS.bits())
                {
                    let thread = zx::Thread::from_handle(thread_handle);
                    match thread.wait_handle(
                        zx::Signals::THREAD_SUSPENDED,
                        zx::MonotonicInstant::after(zx::Duration::INFINITE),
                    ) {
                        Err(e) => {
                            tracing::warn!("Error waiting for task suspension: {:?}", e);
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
