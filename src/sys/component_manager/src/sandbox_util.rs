// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::{ComponentInstance, WeakComponentInstance},
        routing::router::{Completer, Request, Routable, Router},
    },
    ::routing::{capability_source::CapabilitySource, policy::GlobalPolicyChecker},
    cm_types::Name,
    cm_util::WeakTaskGroup,
    fidl::{
        endpoints::{ProtocolMarker, RequestStream},
        epitaph::ChannelEpitaphExt,
        AsyncChannel,
    },
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx},
    futures::future::BoxFuture,
    lazy_static::lazy_static,
    sandbox::{AnyCapability, Capability, Dict, ErasedCapability, Message, Open, Receiver, Sender},
    std::iter,
    std::sync::{self, Arc},
    tracing::{info, warn},
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path},
};

lazy_static! {
    static ref RECEIVER: Name = "receiver".parse().unwrap();
    static ref ROUTER: Name = "router".parse().unwrap();
    static ref SENDER: Name = "sender".parse().unwrap();
}

pub fn take_handle_as_stream<P: ProtocolMarker>(channel: zx::Channel) -> P::RequestStream {
    let channel = AsyncChannel::from_channel(channel);
    P::RequestStream::from_channel(channel)
}

pub trait ProtocolPayloadExt {
    /// If the protocol connection request requires serving the `fuchsia.io/Node`
    /// protocol, serve that internally and return `None`. Otherwise, handle the
    /// connection flags such as `DESCRIBE` and return the server endpoint such
    /// that a custom FIDL protocol may be served on it.
    fn unwrap_server_end_or_serve_node(self) -> Option<zx::Channel>;
}

impl ProtocolPayloadExt for fsandbox::ProtocolPayload {
    fn unwrap_server_end_or_serve_node(self) -> Option<zx::Channel> {
        let server_end_return = Arc::new(sync::Mutex::new(None));
        let server_end_return_clone = server_end_return.clone();
        let service = vfs::service::endpoint(
            move |_scope: ExecutionScope, server_end: fuchsia_async::Channel| {
                let mut server_end_return = server_end_return_clone.lock().unwrap();
                *server_end_return = Some(server_end.into_zx_channel());
            },
        );
        let flags = self.flags;
        let server_end = self.channel;
        service.open(ExecutionScope::new(), flags, Path::dot(), server_end.into());

        let mut server_end_return = server_end_return.lock().unwrap();
        if let Some(server_end) = server_end_return.take() {
            Some(server_end)
        } else {
            None
        }
    }
}

// TODO: use the `Name` type in `Dict`, so that Dicts aren't holding duplicate strings.

pub trait DictExt {
    fn get_or_insert_sub_dict<'a>(&self, path: impl Iterator<Item = &'a str>) -> Dict;

    /// Returns the capability at the path, if it exists. Returns `None` if path is empty.
    fn get_capability<'a, C>(&self, path: impl Iterator<Item = &'a str>) -> Option<C>
    where
        C: ErasedCapability + Capability;

    /// Returns a router configured to route at `path`, if a [Routable]
    /// capability exists at `path.first()`. / Returns None otherwise.
    fn get_routable<'a, C>(&self, path: impl DoubleEndedIterator<Item = &'a str>) -> Option<Router>
    where
        C: ErasedCapability + Capability + Routable;

    /// Inserts the capability at the path. Intermediary dictionaries are created as needed.
    fn insert_capability<'a, C>(&self, path: impl Iterator<Item = &'a str>, capability: C)
    where
        C: ErasedCapability + Capability;

    /// Removes the capability at the path, if it exists.
    fn remove_capability<'a>(&self, path: impl Iterator<Item = &'a str>);
}

impl DictExt for Dict {
    fn get_or_insert_sub_dict<'a>(&self, mut path: impl Iterator<Item = &'a str>) -> Dict {
        let Some(next_name) = path.next() else { return self.clone() };
        let sub_dict: Dict = self
            .lock_entries()
            .entry(next_name.to_string())
            .or_insert(Box::new(Dict::new()))
            .clone()
            .try_into()
            .unwrap();
        sub_dict.get_or_insert_sub_dict(path)
    }

    fn get_capability<'a, C>(&self, mut path: impl Iterator<Item = &'a str>) -> Option<C>
    where
        C: ErasedCapability + Capability,
    {
        let Some(mut current_name) = path.next() else { return None };
        let mut current_dict = self.clone();
        loop {
            match path.next() {
                Some(next_name) => {
                    // Lifetimes are weird here with the MutexGuard, so we do this in two steps
                    let sub_dict = current_dict
                        .lock_entries()
                        .get(&current_name.to_string())
                        .and_then(|value| value.clone().try_into().ok())?;
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => {
                    return current_dict
                        .lock_entries()
                        .get(&current_name.to_string())
                        .cloned()
                        .and_then(|v| v.try_into().ok());
                }
            }
        }
    }

    fn get_routable<'a, C>(
        &self,
        mut path: impl DoubleEndedIterator<Item = &'a str>,
    ) -> Option<Router>
    where
        C: ErasedCapability + Capability + Routable,
    {
        let first = path.next().unwrap();
        let Some(capability) = self.get_capability::<C>(iter::once(first)) else {
            return None;
        };
        Some(Router::from_routable(capability).with_path(path))
    }

    fn insert_capability<'a, C>(&self, mut path: impl Iterator<Item = &'a str>, capability: C)
    where
        C: ErasedCapability + Capability,
    {
        let mut current_name = path.next().expect("path must be non-empty");
        let mut current_dict = self.clone();
        loop {
            match path.next() {
                Some(next_name) => {
                    // Lifetimes are weird here with the MutexGuard, so we do this in two steps
                    let sub_dict = current_dict
                        .lock_entries()
                        .entry(current_name.to_string())
                        .or_insert(Box::new(Dict::new()))
                        .clone()
                        .try_into()
                        .unwrap();
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => {
                    current_dict
                        .lock_entries()
                        .insert(current_name.to_string(), Box::new(capability));
                    return;
                }
            }
        }
    }

    fn remove_capability<'a>(&self, mut path: impl Iterator<Item = &'a str>) {
        let mut current_name = path.next().expect("path must be non-empty");
        let mut current_dict = self.clone();
        loop {
            match path.next() {
                Some(next_name) => {
                    let sub_dict = current_dict
                        .lock_entries()
                        .get(&current_name.to_string())
                        .and_then(|value| value.clone().try_into().ok());
                    if sub_dict.is_none() {
                        // The capability doesn't exist, there's nothing to remove.
                        return;
                    }
                    current_dict = sub_dict.unwrap();
                    current_name = next_name;
                }
                None => {
                    current_dict.lock_entries().remove(&current_name.to_string());
                    return;
                }
            }
        }
    }
}

// This is an adaptor from a `Sender<Message>` to `Router`.
//
// When the router receives a message, it will return an `Open` capability that will
// send a message with a pipelined server endpoint via the `Sender`, along with
// attribution information in the router `request`.
//
// TODO(b/310741884): This is a temporary adaptor to transition us from a one-step
// capability request to two-steps (route and open). At the end of the transition,
// `Sender<Message>` and `Receiver<Message>` would disappear. We'll only be left
// with `Open`, which may represent a protocol capability.
pub fn new_terminating_router(sender: Sender<WeakComponentInstance>) -> Router {
    Router::new(move |request: Request, completer: Completer| {
        let sender = sender.clone();
        let target = request.target.clone();
        let open_fn = move |_scope: ExecutionScope,
                            flags: fio::OpenFlags,
                            path: Path,
                            server_end: zx::Channel| {
            // TODO(b/310741884): Right now we are haphazardly validating the
            // path, but this operation should be handled automatically inside
            // an `Open` which represents a protocol capability. To do that, a
            // capability provider need to provide capabilities by vending
            // `Open` objects.
            //
            // Furthermore, once we route other capability types via bedrock,
            // sometimes those types do want to carry non-empty paths. The
            // `Open` signature will provide a uniform interface for both.
            if !path.is_empty() {
                let moniker = &target.moniker;
                warn!(
                    "{moniker} accessed a protocol capability with non-empty path {path:?}. \
                This is not supported."
                );
                let _ = server_end.close_with_epitaph(zx::Status::NOT_DIR);
                return;
            }
            if let Err(_e) = sender.send(Message {
                payload: fsandbox::ProtocolPayload { channel: server_end.into(), flags },
                target: target.clone(),
            }) {
                info!("failed to send capability: receiver has been destroyed");
            }
        };
        let open = Open::new(open_fn, fio::DirentType::Service);
        let open = Box::new(open) as AnyCapability;
        open.route(request, completer);
    })
}

/// Waits for a new message on a receiver, and launches a new async task on a `WeakTaskGroup` to
/// handle each new message from the receiver.
pub struct LaunchTaskOnReceive {
    receiver: Receiver<WeakComponentInstance>,
    task_to_launch: Arc<
        dyn Fn(zx::Channel, WeakComponentInstance) -> BoxFuture<'static, Result<(), anyhow::Error>>
            + Sync
            + Send
            + 'static,
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
        receiver: Receiver<WeakComponentInstance>,
        policy: Option<(GlobalPolicyChecker, CapabilitySource<ComponentInstance>)>,
        task_to_launch: Arc<
            dyn Fn(
                    zx::Channel,
                    WeakComponentInstance,
                ) -> BoxFuture<'static, Result<(), anyhow::Error>>
                + Sync
                + Send
                + 'static,
        >,
    ) -> Self {
        Self { receiver, task_to_launch, task_group, policy, task_name: task_name.into() }
    }

    pub async fn run(self) {
        while let Some(message) = self.receiver.receive().await {
            if let Some((policy_checker, capability_source)) = &self.policy {
                if let Err(_e) =
                    policy_checker.can_route_capability(&capability_source, &message.target.moniker)
                {
                    // The `can_route_capability` function above will log an error, so we don't
                    // have to.
                    let _ = message.payload.channel.close_with_epitaph(zx::Status::ACCESS_DENIED);
                    continue;
                }
            }

            let Some(server_end) = message.payload.unwrap_server_end_or_serve_node() else {
                continue;
            };
            let fut = (self.task_to_launch)(server_end, message.target);
            let task_name = self.task_name.clone();
            self.task_group.spawn(async move {
                if let Err(error) = fut.await {
                    warn!(%error, "{} failed", task_name);
                }
            });
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl::endpoints::ClientEnd;
    use futures::StreamExt;
    use std::iter;

    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_node_reference_and_describe() {
        let (receiver, sender) = Receiver::<()>::new();
        let open: Open = sender.into();
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(
            scope,
            fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DESCRIBE,
            ".",
            server_end,
        );
        let message = receiver.receive().await.unwrap();

        // We never get the channel because it was intercepted by the VFS.
        assert_matches!(message.payload.unwrap_server_end_or_serve_node(), None);

        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        let result = node.take_event_stream().next().await.unwrap();
        assert_matches!(
            result,
            Ok(fio::NodeEvent::OnOpen_ { s, info })
            if s == zx::Status::OK.into_raw()
            && *info.as_ref().unwrap().as_ref() == fio::NodeInfoDeprecated::Service(fio::Service {})
        );
    }

    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_describe() {
        let (receiver, sender) = Receiver::<()>::new();
        let open: Open = sender.into();
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::DESCRIBE, ".", server_end);
        let message = receiver.receive().await.unwrap();

        // The VFS should send the DESCRIBE event, then hand us the channel.
        assert_matches!(message.payload.unwrap_server_end_or_serve_node(), Some(_));

        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        let result = node.take_event_stream().next().await.unwrap();
        assert_matches!(
            result,
            Ok(fio::NodeEvent::OnOpen_ { s, info })
            if s == zx::Status::OK.into_raw()
            && *info.as_ref().unwrap().as_ref() == fio::NodeInfoDeprecated::Service(fio::Service {})
        );
    }

    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_empty() {
        let (receiver, sender) = Receiver::<()>::new();
        let open: Open = sender.into();
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), ".", server_end);
        let message = receiver.receive().await.unwrap();

        // The VFS should not send any event, but directly hand us the channel.
        assert_matches!(message.payload.unwrap_server_end_or_serve_node(), Some(_));

        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        assert_matches!(node.take_event_stream().next().await, None);
    }

    #[fuchsia::test]
    async fn get_capability() {
        let sub_dict = Dict::new();
        sub_dict.lock_entries().insert("bar".to_string(), Box::new(Dict::new()));
        let (receiver, _) = Receiver::<()>::new();
        sub_dict.lock_entries().insert("baz".to_string(), Box::new(receiver));

        let test_dict = Dict::new();
        test_dict.lock_entries().insert("foo".to_string(), Box::new(sub_dict));

        assert!(test_dict.get_capability::<Dict>(iter::empty()).is_none());
        assert!(test_dict.get_capability::<Dict>(iter::once("nonexistent")).is_none());
        assert!(test_dict.get_capability::<Dict>(iter::once("foo")).is_some());
        assert!(test_dict.get_capability::<Router>(iter::once("foo")).is_none());
        assert!(test_dict.get_capability::<Dict>(["foo", "bar"].into_iter()).is_some());
        assert!(test_dict.get_capability::<Dict>(["foo", "nonexistent"].into_iter()).is_none());
        assert!(test_dict.get_capability::<Dict>(["foo", "baz"].into_iter()).is_none());
        assert!(test_dict.get_capability::<Receiver<()>>(["foo", "baz"].into_iter()).is_some());
    }

    #[fuchsia::test]
    async fn insert_capability() {
        let test_dict = Dict::new();
        test_dict.insert_capability(["foo", "bar"].into_iter(), Dict::new());
        assert!(test_dict.get_capability::<Dict>(["foo", "bar"].into_iter()).is_some());

        let (receiver, _) = Receiver::<()>::new();
        test_dict.insert_capability(["foo", "baz"].into_iter(), receiver);
        assert!(test_dict.get_capability::<Receiver<()>>(["foo", "baz"].into_iter()).is_some());
    }

    #[fuchsia::test]
    async fn remove_capability() {
        let test_dict = Dict::new();
        test_dict.insert_capability(["foo", "bar"].into_iter(), Dict::new());
        assert!(test_dict.get_capability::<Dict>(["foo", "bar"].into_iter()).is_some());

        test_dict.remove_capability(["foo", "bar"].into_iter());
        assert!(test_dict.get_capability::<Dict>(["foo", "bar"].into_iter()).is_none());
        assert!(test_dict.get_capability::<Dict>(["foo"].into_iter()).is_some());

        test_dict.remove_capability(iter::once("foo"));
        assert!(test_dict.get_capability::<Dict>(["foo"].into_iter()).is_none());
    }
}
