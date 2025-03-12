// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::suspend::{ResumeEvents, KERNEL_SIGNAL, RUNNER_SIGNAL};
use anyhow::{anyhow, Error};
use fuchsia_async as fasync;
use fuchsia_sync::Mutex;
use futures::FutureExt;
use log::warn;
use std::cell::RefCell;
use std::mem::MaybeUninit;
use std::rc::Rc;
use std::sync::Arc;
use zx::AsHandleRef;

/// `ChannelProxy` is used to proxy messages on a `zx::Channel` between the Starnix
/// container and the outside world. This allows the Starnix runner to wake the container
/// on incoming messages.
///
/// [platform component] <-- remote_channel --> [Starnix runner] <-- container_channel --> [Starnix container]
pub struct ChannelProxy {
    /// The channel that is connected to the container component.
    pub container_channel: zx::Channel,

    /// The channel that is connected to a peer outside of the container component.
    pub remote_channel: zx::Channel,

    /// The resume event that is signaled when messages are proxied into the container.
    pub resume_event: zx::EventPair,

    /// Human readable name for the thing that is being proxied.
    pub name: String,
}

// `WaitReturn` is used to indicate which proxy endpoint caused the wait to complete.
#[derive(Debug)]
enum WaitReturn {
    Container,
    Remote,
}

/// The Zircon role name that is applied to proxy threads.
const PROXY_ROLE_NAME: &str = "fuchsia.starnix.runner.proxy";

/// Starts a thread that listens for new proxies and runs `start_proxy` on each.
pub fn run_proxy_thread(
    new_proxies: async_channel::Receiver<(ChannelProxy, Arc<Mutex<ResumeEvents>>)>,
) {
    let _ = std::thread::Builder::new().name("proxy_thread".to_string()).spawn(move || {
        if let Err(e) = fuchsia_scheduler::set_role_for_this_thread(PROXY_ROLE_NAME) {
            warn!(e:%; "failed to set thread role");
        }
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
    let proxy_name = proxy.name.as_str();
    trace_instant(c"starnix_runner:start_proxy:loop:enter", proxy_name);

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
            trace_duration(c"starnix_runner:start_proxy:wait_for_messages", proxy_name);
            let result = futures::select! {
                res = container_wait => {
                    trace_instant(c"starnix_runner:start_proxy:container_readable", proxy_name);
                    res.map(|s| (s, WaitReturn::Container))
                },
                res = remote_wait => {
                    trace_instant(c"starnix_runner:start_proxy:remote_readable", proxy_name);
                    res.map(|s| (s, WaitReturn::Remote))
                },
            };

            match result {
                Ok(result) => result,
                Err(e) => {
                    trace_instant(c"starnix_runner:start_proxy:result:error", proxy_name);
                    log::warn!("Failed to wait on proxied channels in runner: {:?}", e);
                    break 'outer;
                }
            }
        };

        // Forward messages in both directions. Only messages that are entering the container
        // should signal `proxy.resume_event`, since those are the only messages that should
        // wake the container if it's suspended.
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

    trace_instant(c"starnix_runner:start_proxy:loop:exit", proxy_name);
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
    trace_duration(c"starnix_runner:forward_message", name);

    if signals.contains(zx::Signals::CHANNEL_READABLE) {
        let (actual_bytes, actual_handles) = {
            match read_channel.read_uninit(bytes, handles) {
                zx::ChannelReadResult::Ok(r) => r,
                _ => return Err(anyhow!("Failed to read from channel")),
            }
        };

        if let Some(event) = event {
            // Signal event with `RUNNER_SIGNAL`, indicating that an event is being sent to
            // the kernel.
            let (clear_mask, set_mask) = (KERNEL_SIGNAL, RUNNER_SIGNAL);
            event.signal_handle(clear_mask, set_mask)?;
            trace_instant(c"starnix_runner:forward_message:runner_signal_raised", name);
        }

        write_channel.write(actual_bytes, actual_handles)?;

        if let Some(event) = event {
            // Wait for the kernel endpoint to signal that the event has been handled, and
            // that it is now safe to suspend the container again.
            trace_duration(c"starnix_runner:forward_message:wait_for_kernel", name);

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

            // Clear the kernel signal for this message before continuing.
            let (clear_mask, set_mask) = (KERNEL_SIGNAL, zx::Signals::NONE);
            event.signal_handle(clear_mask, set_mask)?;
            trace_instant(c"starnix_runner:forward_message:kernel_signal_cleared", name);
        }
    }

    // It is important to check for peer closed after readable, in order to flush any
    // remaining messages in the proxied channel.
    if signals.contains(zx::Signals::CHANNEL_PEER_CLOSED) {
        Err(anyhow!("Proxy peer was closed"))
    } else {
        Ok(())
    }
}

fn trace_duration(event: &'static std::ffi::CStr, name: &str) {
    fuchsia_trace::duration!(c"power", event, "name" => name);
}

fn trace_instant(event: &'static std::ffi::CStr, name: &str) {
    fuchsia_trace::instant!(
        c"power",
        event,
        fuchsia_trace::Scope::Process,
        "name" => name
    );
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
