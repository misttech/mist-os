// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod kernels;

use anyhow::{anyhow, Error};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy, ServerEnd};
use fidl::{HandleBased, Peered};
use fuchsia_component::client as fclient;
use fuchsia_sync::Mutex;
use futures::{FutureExt, TryStreamExt};
use kernels::Kernels;
use log::{debug, warn};
use rand::Rng;
use std::cell::RefCell;
use std::future::Future;
use std::mem::MaybeUninit;
use std::rc::Rc;
use std::sync::Arc;
use zx::{AsHandleRef, Task};
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_io as fio,
    fidl_fuchsia_starnix_container as fstarnix, fidl_fuchsia_starnix_runner as fstarnixrunner,
    fuchsia_async as fasync,
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

/// The signal that the kernel raises to indicate that it's awake.
const AWAKE_SIGNAL: zx::Signals = zx::Signals::USER_0;

/// The signal that the kernel raises to indicate that it's suspended.
const ASLEEP_SIGNAL: zx::Signals = zx::Signals::USER_1;

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
        let (controller_proxy, controller_server_end) = fidl::endpoints::create_proxy();
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
            fidl::endpoints::create_proxy();
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

struct ResumeEvent {
    event: zx::EventPair,
    name: String,
}

#[derive(Default)]
pub struct ResumeEvents {
    events: std::collections::HashMap<zx::Koid, ResumeEvent>,
}

#[derive(Default)]
pub struct SuspendContext {
    resume_events: Arc<Mutex<ResumeEvents>>,
    wake_watchers: Arc<Mutex<Vec<zx::EventPair>>>,
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
    let (directory_proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
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

async fn suspend_container(
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
    let _suspend_handles = match suspend_kernel(&container_job).await {
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

    let suspend_start = zx::BootInstant::get();

    if let Some(wake_locks) = payload.wake_locks {
        match wake_locks.wait_handle(zx::Signals::EVENT_SIGNALED, zx::MonotonicInstant::ZERO) {
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
        let watchers = suspend_context.wake_watchers.lock();
        for event in watchers.iter() {
            let (clear_mask, set_mask) = (AWAKE_SIGNAL, ASLEEP_SIGNAL);
            event.signal_peer(clear_mask, set_mask)?;
        }
    }
    kernels.drop_wake_lease(&container_job)?;

    let resume_events = suspend_context.resume_events.lock();
    let mut wait_items: Vec<zx::WaitItem<'_>> = resume_events
        .events
        .iter()
        .map(|(_koid, event)| zx::WaitItem {
            handle: event.event.as_handle_ref(),
            waitfor: RUNNER_SIGNAL,
            pending: zx::Signals::empty(),
        })
        .collect();

    // TODO: We will likely have to handle a larger number of wake sources in the
    // future, at which point we may want to consider a Port-based approach. This
    // would also allow us to unblock this thread.
    {
        fuchsia_trace::duration!(c"power", c"starnix-runner:waiting-on-container-wake");
        if wait_items.len() > 0 {
            match zx::object_wait_many(&mut wait_items, zx::MonotonicInstant::INFINITE) {
                Ok(_) => (),
                Err(e) => {
                    warn!("error waiting for wake event {:?}", e);
                }
            };
        }
    }

    for wait_item in &wait_items {
        if wait_item.pending.contains(RUNNER_SIGNAL) {
            let koid = wait_item.handle.get_koid().unwrap();
            if let Some(event) = resume_events.events.get(&koid) {
                log::info!("Woke from sleep for: {}", event.name);
            }
        }
    }

    kernels.acquire_wake_lease(&container_job).await?;

    let watchers = suspend_context.wake_watchers.lock();
    for event in watchers.iter() {
        let (clear_mask, set_mask) = (ASLEEP_SIGNAL, AWAKE_SIGNAL);
        event.signal_peer(clear_mask, set_mask)?;
    }

    Ok(Ok(fstarnixrunner::ManagerSuspendContainerResponse {
        suspend_time: Some((zx::BootInstant::get() - suspend_start).into_nanos()),
        ..Default::default()
    }))
}

pub async fn serve_starnix_manager(
    mut stream: fstarnixrunner::ManagerRequestStream,
    suspend_context: Arc<SuspendContext>,
    kernels: &Kernels,
    sender: &async_channel::Sender<(ChannelProxy, Arc<Mutex<ResumeEvents>>)>,
) -> Result<(), Error> {
    while let Some(event) = stream.try_next().await? {
        match event {
            fstarnixrunner::ManagerRequest::SuspendContainer { payload, responder, .. } => {
                let response = suspend_container(payload, &suspend_context, kernels).await?;
                if let Err(e) = match response {
                    Ok(o) => responder.send(Ok(&o)),
                    Err(e) => responder.send(Err(e)),
                } {
                    warn!("error replying to suspend request: {e}");
                }
            }
            fstarnixrunner::ManagerRequest::ProxyWakeChannel { payload, .. } => {
                let fstarnixrunner::ManagerProxyWakeChannelRequest {
                    // TODO: Handle more than one container.
                    container_job: Some(_container_job),
                    remote_channel: Some(remote_channel),
                    container_channel: Some(container_channel),
                    resume_event: Some(resume_event),
                    name: Some(name),
                    ..
                } = payload
                else {
                    continue;
                };

                let proxy = ChannelProxy {
                    container_channel,
                    remote_channel,
                    resume_event,
                    name: name.clone(),
                };
                suspend_context.resume_events.lock().events.insert(
                    proxy.resume_event.get_koid().unwrap(),
                    ResumeEvent {
                        event: proxy
                            .resume_event
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("failed"),
                        name: name,
                    },
                );
                sender.try_send((proxy, suspend_context.resume_events.clone())).unwrap();
            }
            fstarnixrunner::ManagerRequest::RegisterWakeWatcher { payload, responder } => {
                if let Some(watcher) = payload.watcher {
                    let (clear_mask, set_mask) = (ASLEEP_SIGNAL, AWAKE_SIGNAL);
                    watcher.signal_peer(clear_mask, set_mask)?;

                    suspend_context.wake_watchers.lock().push(watcher);
                }
                if let Err(e) = responder.send() {
                    warn!("error registering power watcher: {e}");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub struct ChannelProxy {
    /// The channel that is connected to the container component.
    container_channel: zx::Channel,

    /// The channel that is connected to a peer outside of the container component.
    remote_channel: zx::Channel,

    /// The resume event that is signaled when messages are proxied into the container.
    resume_event: zx::EventPair,

    /// Human readable name for the thing that is being proxied.
    name: String,
}

/// Starts a thread that listens for new proxies and runs `start_proxy` on each.
pub fn run_proxy_thread(
    new_proxies: async_channel::Receiver<(ChannelProxy, Arc<Mutex<ResumeEvents>>)>,
) {
    let _ = std::thread::Builder::new().name("proxy_thread".to_string()).spawn(move || {
        let mut executor = fasync::LocalExecutor::new();
        executor.run_singlethreaded(async move {
            let mut tasks = fasync::TaskGroup::new();
            let bounce_bytes = Rc::new(RefCell::new(
                [MaybeUninit::uninit(); zx::sys::ZX_CHANNEL_MAX_MSG_BYTES as usize],
            ));
            let bounce_handles = Rc::new(RefCell::new(
                [const { MaybeUninit::uninit() }; zx::sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize],
            ));
            while let Ok((proxy, events)) = new_proxies.recv().await {
                let bytes_clone = bounce_bytes.clone();
                let handles_clone = bounce_handles.clone();
                tasks.local(start_proxy(proxy, events, bytes_clone, handles_clone));
            }
        });
    });
}

/// Starts a task that proxies messages between `proxy.container_channel` and
/// `proxy.remote_channel`. The task will exit when either of the channels' peer is closed, or
/// if `proxy.resume_event`'s peer is closed.
///
/// When the task exits, `proxy.resume_event` will be removed from `resume_events`.
async fn start_proxy(
    proxy: ChannelProxy,
    resume_events: Arc<Mutex<ResumeEvents>>,
    bounce_bytes: Rc<RefCell<[MaybeUninit<u8>; zx::sys::ZX_CHANNEL_MAX_MSG_BYTES as usize]>>,
    bounce_handles: Rc<
        RefCell<[MaybeUninit<zx::Handle>; zx::sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize]>,
    >,
) {
    // This enum tells us which wait finished first.
    enum WaitReturn {
        Container,
        Remote,
    }
    'outer: loop {
        // Wait on messages from both the container and remote channel endpoints.
        let mut container_wait = fasync::OnSignals::new(
            proxy.container_channel.as_handle_ref(),
            zx::Signals::CHANNEL_READABLE | zx::Signals::CHANNEL_PEER_CLOSED,
        )
        .fuse();
        let mut remote_wait = fasync::OnSignals::new(
            proxy.remote_channel.as_handle_ref(),
            zx::Signals::CHANNEL_READABLE | zx::Signals::CHANNEL_PEER_CLOSED,
        )
        .fuse();

        let (signals, finished_wait) = {
            fuchsia_trace::duration!(c"power", c"starnix-runner:proxy-waiting-for-messages", "name" => proxy.name.as_str());
            let result = futures::select! {
                res = container_wait => res.map(|s| (s, WaitReturn::Container)),
                res = remote_wait => res.map(|s| (s, WaitReturn::Remote)),
            };
            match result {
                Ok(result) => result,
                Err(e) => {
                    log::warn!("Failed to wait on proxied channels in runner: {:?}", e);
                    break 'outer;
                }
            }
        };

        // Forward messages in both directions. Only messages that are entering the container
        // should signal `proxy.resume_event`, since those are the only messages that should
        // wake the container if it's suspended.
        fuchsia_trace::duration!(c"power", c"starnix-runner:proxy-forwarding-messages", "name" => proxy.name.as_str());
        let name = proxy.name.as_str();
        let result = match finished_wait {
            WaitReturn::Container => forward_message(
                &signals,
                &proxy.container_channel,
                &proxy.remote_channel,
                None,
                &mut bounce_bytes.borrow_mut(),
                &mut bounce_handles.borrow_mut(),
                name,
            ),
            WaitReturn::Remote => forward_message(
                &signals,
                &proxy.remote_channel,
                &proxy.container_channel,
                Some(&proxy.resume_event),
                &mut bounce_bytes.borrow_mut(),
                &mut bounce_handles.borrow_mut(),
                name,
            ),
        };

        if result.is_err() {
            log::warn!(
                "Proxy failed to forward message {} kernel: {}; {:?}",
                match finished_wait {
                    WaitReturn::Container => "from",
                    WaitReturn::Remote => "to",
                },
                name,
                result,
            );
            break 'outer;
        }
    }

    if let Ok(koid) = proxy.resume_event.get_koid() {
        resume_events.lock().events.remove(&koid);
    }
}

/// Forwards any pending messages on `read_channel` to `write_channel`, if the `wait_item.pending`
/// contains `CHANNEL_READABLE`.
///
/// If `event` is `Some`, it will be signaled with `EVENT_SIGNALED` if a message was read and
/// written.
fn forward_message(
    signals: &zx::Signals,
    read_channel: &zx::Channel,
    write_channel: &zx::Channel,
    event: Option<&zx::EventPair>,
    bytes: &mut [MaybeUninit<u8>; zx::sys::ZX_CHANNEL_MAX_MSG_BYTES as usize],
    handles: &mut [MaybeUninit<zx::Handle>; zx::sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize],
    name: &str,
) -> Result<(), Error> {
    if signals.contains(zx::Signals::CHANNEL_READABLE) {
        debug!("runner_proxy: {}: 1: entry, event={:?}", name, event);
        let (actual_bytes, actual_handles) = match read_channel.read_uninit(bytes, handles) {
            zx::ChannelReadResult::Ok(r) => r,
            _ => return Err(anyhow!("Failed to read from channel")),
        };

        if let Some(event) = event {
            // Signal event with `RUNNER_SIGNAL`, indicating that an event is being sent to
            // the kernel.
            let (clear_mask, set_mask) = (KERNEL_SIGNAL, RUNNER_SIGNAL);
            event.signal_handle(clear_mask, set_mask)?;
            debug!("runner_proxy: {}: 4: K=0, R=1", name);
        }

        write_channel.write(actual_bytes, actual_handles)?;

        if let Some(event) = event {
            debug!("{}: 5: wait for K=1", name);
            // Wait for the kernel endpoint to signal that the event has been handled, and
            // that it is now safe to suspend the container again.
            match event.wait_handle(
                KERNEL_SIGNAL | zx::Signals::EVENTPAIR_PEER_CLOSED,
                zx::MonotonicInstant::INFINITE,
            ) {
                Ok(signals) => {
                    if signals.contains(zx::Signals::EVENTPAIR_PEER_CLOSED) {
                        return Err(anyhow!("Proxy eventpair was closed"));
                    }
                }
                Err(e) => {
                    log::warn!("Failed to wait on proxied channels in runner: {:?}", e);
                    return Err(anyhow!("Failed to wait on signal from kernel"));
                }
            };
            debug!("runner_proxy: {} 6: K=1, R=0", name);
            // Clear the kernel signal for this message before continuing.
            let (clear_mask, set_mask) = (KERNEL_SIGNAL, zx::Signals::NONE);
            event.signal_handle(clear_mask, set_mask)?;

            debug!("runner_proxy: {}: 7: K=0, R=0", name);
        }
        debug!("runner_proxy: {}: 9: loop done: event={:?}", name, event);
    }
    if signals.contains(zx::Signals::CHANNEL_PEER_CLOSED) {
        Err(anyhow!("Proxy peer was closed"))
    } else {
        Ok(())
    }
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
                if let Ok(thread) = process.get_child(&thread_koid, zx::Rights::SAME_RIGHTS) {
                    match thread.wait_handle(
                        zx::Signals::THREAD_SUSPENDED,
                        zx::MonotonicInstant::after(zx::MonotonicDuration::INFINITE),
                    ) {
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

#[cfg(test)]
mod test {
    use super::{fasync, start_proxy, ChannelProxy};
    use std::cell::RefCell;
    use std::mem::MaybeUninit;
    use std::rc::Rc;

    fn run_proxy_for_test(proxy: ChannelProxy) -> fasync::Task<()> {
        let bounce_bytes = Rc::new(RefCell::new(
            [MaybeUninit::uninit(); zx::sys::ZX_CHANNEL_MAX_MSG_BYTES as usize],
        ));
        let bounce_handles = Rc::new(RefCell::new(
            [const { MaybeUninit::uninit() }; zx::sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize],
        ));
        fasync::Task::local(start_proxy(proxy, Default::default(), bounce_bytes, bounce_handles))
    }

    #[fuchsia::test]
    async fn test_peer_closed_kernel() {
        let (local_client, local_server) = zx::Channel::create();
        let (remote_client, remote_server) = zx::Channel::create();
        let (resume_event, _local_resume_event) = zx::EventPair::create();

        let channel_proxy = ChannelProxy {
            container_channel: local_server,
            remote_channel: remote_client,
            resume_event,
            name: "test".to_string(),
        };
        let _task = run_proxy_for_test(channel_proxy);

        std::mem::drop(local_client);

        fasync::OnSignals::new(remote_server, zx::Signals::CHANNEL_PEER_CLOSED).await.unwrap();
    }

    #[fuchsia::test]
    async fn test_peer_closed_remote() {
        let (local_client, local_server) = zx::Channel::create();
        let (remote_client, remote_server) = zx::Channel::create();
        let (resume_event, _local_resume_event) = zx::EventPair::create();

        let channel_proxy = ChannelProxy {
            container_channel: local_server,
            remote_channel: remote_client,
            resume_event,
            name: "test".to_string(),
        };
        let _task = run_proxy_for_test(channel_proxy);

        std::mem::drop(remote_server);

        fasync::OnSignals::new(local_client, zx::Signals::CHANNEL_PEER_CLOSED).await.unwrap();
    }

    #[fuchsia::test]
    async fn test_peer_closed_event() {
        let (local_client, local_server) = zx::Channel::create();
        let (remote_client, remote_server) = zx::Channel::create();
        let (resume_event, local_resume_event) = zx::EventPair::create();

        let channel_proxy = ChannelProxy {
            container_channel: local_server,
            remote_channel: remote_client,
            resume_event,
            name: "test".to_string(),
        };
        let _task = run_proxy_for_test(channel_proxy);

        std::mem::drop(local_resume_event);

        assert!(remote_server.write(&[0x0, 0x1, 0x2], &mut []).is_ok());

        fasync::OnSignals::new(local_client, zx::Signals::CHANNEL_PEER_CLOSED).await.unwrap();
    }
}
