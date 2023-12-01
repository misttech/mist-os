// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::component::{ComponentInstance, WeakComponentInstance},
    ::routing::{
        capability_source::CapabilitySource, policy::GlobalPolicyChecker, Completer, Request,
        Router,
    },
    async_trait::async_trait,
    cm_types::Name,
    cm_util::WeakTaskGroup,
    fidl::{
        endpoints::{ProtocolMarker, RequestStream},
        epitaph::ChannelEpitaphExt,
        AsyncChannel,
    },
    fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{
        future::BoxFuture,
        stream::{FuturesUnordered, StreamExt},
    },
    lazy_static::lazy_static,
    moniker::Moniker,
    sandbox::{AnyCapability, Capability, Dict, Receiver},
    std::sync::Arc,
    tracing::warn,
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path},
};

lazy_static! {
    static ref AVAILABILITY: Name = "availability".parse().unwrap();
    static ref RECEIVER: Name = "receiver".parse().unwrap();
    static ref ROUTER: Name = "router".parse().unwrap();
    static ref SENDER: Name = "sender".parse().unwrap();
}

#[derive(Debug)]
pub struct Message {
    pub handle: zx::Handle,
    pub flags: fio::OpenFlags,
    pub target: WeakComponentInstance,
}

impl Message {
    pub fn take_handle_as_stream<P: ProtocolMarker>(self) -> P::RequestStream {
        let channel = AsyncChannel::from_channel(zx::Channel::from(self.handle))
            .expect("failed to convert handle into async channel");
        P::RequestStream::from_channel(channel)
    }
}

impl Capability for Message {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        (self.handle, None)
    }
}

impl From<zx::Handle> for Message {
    fn from(handle: zx::Handle) -> Self {
        Self {
            handle,
            // TODO
            flags: fio::OpenFlags::empty(),
            // TODO
            target: WeakComponentInstance::invalid(),
        }
    }
}

impl TryFrom<AnyCapability> for Message {
    type Error = ();

    fn try_from(_from: AnyCapability) -> Result<Self, Self::Error> {
        panic!("TODO: unimplemented");
    }
}

// TODO: use the `Name` type in `Dict`, so that Dicts aren't holding duplicate strings.

#[async_trait]
pub trait DictExt {
    fn get_protocol<'a>(&'a self, name: &Name) -> Option<CapabilityDict<'a>>;
    fn get_protocol_mut<'a>(&'a mut self, name: &Name) -> Option<CapabilityDictMut<'a>>;
    fn get_or_insert_protocol_mut<'a>(&'a mut self, name: &Name) -> CapabilityDictMut<'a>;
    async fn peek_receivers(&self) -> Option<(Name, Moniker)>;
    async fn read_receivers(&self) -> Option<(Name, Message)>;
}

#[async_trait]
impl DictExt for Dict {
    fn get_protocol<'a>(&'a self, name: &Name) -> Option<CapabilityDict<'a>> {
        self.entries
            .get(&name.as_str().to_string())
            .and_then(|value| value.try_into().ok())
            .map(|inner| CapabilityDict { inner })
    }

    fn get_protocol_mut<'a>(&'a mut self, name: &Name) -> Option<CapabilityDictMut<'a>> {
        self.entries
            .get_mut(&name.as_str().to_string())
            .and_then(|value| value.try_into().ok())
            .map(|inner| CapabilityDictMut { inner })
    }

    fn get_or_insert_protocol_mut<'a>(&'a mut self, name: &Name) -> CapabilityDictMut<'a> {
        CapabilityDictMut {
            inner: self
                .entries
                .entry(name.as_str().to_string())
                .or_insert(Box::new(Dict::new()))
                .try_into()
                .unwrap(),
        }
    }

    /// Waits for any Receivers to become readable.
    ///
    /// Once that happens, returns the name of the Dict that Receiver was in and the moniker that
    /// sent the message. Returns `None` if there are no Receivers in this Dict.
    ///
    /// Does not remove messages from Receivers.
    async fn peek_receivers(&self) -> Option<(Name, Moniker)> {
        let mut futures_unordered = FuturesUnordered::new();
        for cap_name in self.entries.keys() {
            let cap_dict = self.get_protocol(&cap_name.parse().unwrap()).unwrap();
            if cap_dict.get_receiver().is_some() {
                futures_unordered.push(async move {
                    // It would be great if we could return the value from the `peek` call here,
                    // but the lifetimes don't work out. Let's block on the peek call, and then
                    // return the `cap_dict` so we can access the `peek` value again outside
                    // of the `FuturesUnordered`.
                    let _ = cap_dict.get_receiver().unwrap().peek().await;
                    (cap_name, cap_dict)
                });
            }
        }
        if futures_unordered.is_empty() {
            return None;
        }
        let (name, cap_dict) =
            futures_unordered.next().await.expect("FuturesUnordered is not empty");
        let message = cap_dict.get_receiver().unwrap().peek().await;
        return Some((name.parse().unwrap(), message.target.moniker.clone()));
    }

    /// Reads messages from Receivers in this Dict.
    ///
    /// Once a message is received, returns the name of the Dict that the Receiver was in and the
    /// message that was received. Returns `None` if there are no Receivers in this Dict.
    async fn read_receivers(&self) -> Option<(Name, Message)> {
        let mut futures_unordered = FuturesUnordered::new();
        for cap_name in self.entries.keys() {
            let cap_dict = self.get_protocol(&cap_name.parse().unwrap()).unwrap();
            if let Some(receiver) = cap_dict.get_receiver() {
                let receiver = receiver.clone();
                futures_unordered.push(async move { (cap_name, receiver.receive().await) });
            }
        }
        if futures_unordered.is_empty() {
            return None;
        }
        let (name, message) =
            futures_unordered.next().await.expect("FuturesUnordered is not empty");
        return Some((name.parse().unwrap(), message));
    }
}

/// A mutable dict for a single capability.
pub struct CapabilityDict<'a> {
    inner: &'a Dict,
}

impl<'a> CapabilityDict<'a> {
    pub fn get_receiver(&self) -> Option<&Receiver<Message>> {
        self.inner.entries.get(&RECEIVER.as_str().to_string()).and_then(|v| v.try_into().ok())
    }

    pub fn get_router(&self) -> Option<&Router> {
        self.inner.entries.get(&ROUTER.as_str().to_string()).and_then(|v| v.try_into().ok())
    }
}

/// A mutable dict for a single capability.
pub struct CapabilityDictMut<'a> {
    inner: &'a mut Dict,
}

impl<'a> CapabilityDictMut<'a> {
    pub fn get_router(&mut self) -> Option<&mut Router> {
        self.inner.entries.get_mut(&ROUTER.as_str().to_string()).and_then(|v| v.try_into().ok())
    }

    pub fn insert_router(&mut self, router: Router) {
        let old_val = self.inner.entries.insert(ROUTER.as_str().to_string(), Box::new(router));
        assert!(old_val.is_none(), "overwrote a router, this shouldn't be possible");
    }

    pub fn remove_router(&mut self) {
        self.inner.entries.remove(&ROUTER.as_str().to_string());
    }

    pub fn get_receiver(&mut self) -> Option<&mut Receiver<Message>> {
        self.inner.entries.get_mut(&RECEIVER.as_str().to_string()).and_then(|v| v.try_into().ok())
    }

    pub fn insert_receiver(&mut self, receiver: Receiver<Message>) {
        let old_val = self.inner.entries.insert(RECEIVER.as_str().to_string(), Box::new(receiver));
        assert!(old_val.is_none(), "overwrote a receiver, this shouldn't be possible");
    }
}

pub fn new_terminating_router(capability_provider: Receiver<Message>) -> Router {
    Router::new(move |_request: Request, completer: Completer| {
        let sender = capability_provider.new_sender();
        // TODO: request has rights and a relative path, we could make a sender that constructs a
        // message with these?
        // TODO: target_moniker in Request is unused, because Message has a reference to the target
        completer.complete(Ok(Box::new(sender)));
    })
}

/// Waits for any Receiver in a Dict to become readable, and calls a closure when that happens.
pub struct DictWaiter {
    _task: fasync::Task<()>,
}

impl DictWaiter {
    pub fn new(
        dict: Dict,
        call_when_dict_is_readable: impl FnOnce(&Name, Moniker) -> BoxFuture<'static, ()>
            + Send
            + 'static,
    ) -> Self {
        Self {
            _task: fasync::Task::spawn(async move {
                if let Some((name, moniker)) = dict.peek_receivers().await {
                    call_when_dict_is_readable(&name, moniker).await
                }
            }),
        }
    }
}

/// Waits for a new message on a receiver, and launches a new async task on a `WeakTaskGroup` to
/// handle each new message from the receiver.
pub struct LaunchTaskOnReceive {
    receiver: Receiver<Message>,
    task_to_launch: Arc<
        dyn Fn(Message) -> BoxFuture<'static, Result<(), anyhow::Error>> + Sync + Send + 'static,
    >,
    // Note that we explicitly need a `WeakTaskGroup` because if our `run` call is scheduled on the
    // same task group as we'll be launching tasks on then if we held a strong reference we would
    // inadvertently give the task group a strong reference to itself and make it un-droppable.
    task_group: WeakTaskGroup,
    policy: Option<(GlobalPolicyChecker, CapabilitySource<ComponentInstance>)>,
    task_name: String,
}

impl LaunchTaskOnReceive {
    pub fn new(
        task_group: WeakTaskGroup,
        task_name: impl Into<String>,
        receiver: Receiver<Message>,
        policy: Option<(GlobalPolicyChecker, CapabilitySource<ComponentInstance>)>,
        task_to_launch: Arc<
            dyn Fn(Message) -> BoxFuture<'static, Result<(), anyhow::Error>>
                + Sync
                + Send
                + 'static,
        >,
    ) -> Self {
        Self { receiver, task_to_launch, task_group, policy, task_name: task_name.into() }
    }

    pub async fn run(self) {
        loop {
            let message = self.receiver.receive().await;
            if let Some((policy_checker, capability_source)) = &self.policy {
                if let Err(_e) =
                    policy_checker.can_route_capability(&capability_source, &message.target.moniker)
                {
                    // The `can_route_capability` function above will log an error, so we don't
                    // have to.
                    let _ = zx::Channel::from(message.handle)
                        .close_with_epitaph(zx::Status::ACCESS_DENIED);
                    continue;
                }
            }
            // The open must be wrapped in a [vfs] to correctly implement the full
            // contract of `fuchsia.io`, including OPEN_FLAGS_DESCRIBE, etc.
            //
            // TODO(fxbug.dev/296309292): This technically does not implement the full
            // contract because it does not handle the path. Service vfs is supposed
            // to reject the request if the path is nonempty. However, the path is
            // currently not delivered in the message.
            let flags = message.flags;
            let target = message.target;
            let server_end = zx::Channel::from(message.handle).into();
            let task_to_launch = self.task_to_launch.clone();
            let task_group = self.task_group.clone();
            let task_name = self.task_name.clone();
            let service = vfs::service::endpoint(
                move |_scope: ExecutionScope, server_end: fuchsia_async::Channel| {
                    let handle = server_end.into_zx_channel().into_handle();
                    let message = Message { handle, flags, target: target.clone() };
                    let fut = (task_to_launch)(message);
                    let task_name = task_name.clone();
                    task_group.spawn(async move {
                        if let Err(error) = fut.await {
                            warn!(%error, "{} failed", task_name);
                        }
                    });
                },
            );
            service.open(ExecutionScope::new(), flags, Path::dot(), server_end);
        }
    }
}
