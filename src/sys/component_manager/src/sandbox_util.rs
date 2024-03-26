// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        model::{
            component::{ComponentInstance, WeakComponentInstance},
            routing::router::{Request, Routable, Router},
        },
        PathBuf,
    },
    ::routing::{
        capability_source::CapabilitySource, component_instance::ComponentInstanceInterface,
        error::RoutingError, policy::GlobalPolicyChecker,
    },
    async_trait::async_trait,
    bedrock_error::BedrockError,
    cm_types::Name,
    cm_util::WeakTaskGroup,
    fidl::{
        endpoints::{ProtocolMarker, RequestStream},
        epitaph::ChannelEpitaphExt,
        AsyncChannel,
    },
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx},
    futures::future::BoxFuture,
    futures::FutureExt,
    sandbox::{Capability, Dict, Message, Sendable, Sender},
    std::iter,
    std::sync::Arc,
    tracing::warn,
};

pub fn take_handle_as_stream<P: ProtocolMarker>(channel: zx::Channel) -> P::RequestStream {
    let channel = AsyncChannel::from_channel(channel);
    P::RequestStream::from_channel(channel)
}

pub trait DictExt {
    fn get_or_insert_sub_dict<'a>(&self, path: impl Iterator<Item = &'a Name>) -> Dict;

    /// Returns the capability at the path, if it exists. Returns `None` if path is empty.
    fn get_capability<'a>(&self, path: impl Iterator<Item = &'a Name>) -> Option<Capability>;

    /// Attempts to walk `path` in `self` until a router is found. If one is, it is then downscoped
    /// to the remaining path. If a router is not found, then a router is returned which will
    /// unconditionally fail routing requests with `error`.
    fn get_router_or_error<'a>(
        &self,
        path: impl DoubleEndedIterator<Item = &'a Name>,
        error: RoutingError,
    ) -> Router;

    /// Inserts the capability at the path. Intermediary dictionaries are created as needed.
    fn insert_capability<'a>(&self, path: impl Iterator<Item = &'a Name>, capability: Capability);

    /// Removes the capability at the path, if it exists.
    fn remove_capability<'a>(&self, path: impl Iterator<Item = &'a Name>);

    /// Looks up the element at `path`. When encountering an intermediate router, use `request`
    /// to request the underlying capability from it. In contrast, `get_capability` will return
    /// `None`.
    async fn get_with_request<'a>(
        &self,
        path: impl Iterator<Item = &'a Name>,
        request: Request,
    ) -> Result<Option<Capability>, BedrockError>;
}

impl DictExt for Dict {
    fn get_or_insert_sub_dict<'a>(&self, mut path: impl Iterator<Item = &'a Name>) -> Dict {
        let Some(next_name) = path.next() else { return self.clone() };
        let sub_dict: Dict = self
            .lock_entries()
            .entry(next_name.clone())
            .or_insert(Capability::Dictionary(Dict::new()))
            .clone()
            .to_dictionary()
            .unwrap();
        sub_dict.get_or_insert_sub_dict(path)
    }

    fn get_capability<'a>(&self, mut path: impl Iterator<Item = &'a Name>) -> Option<Capability> {
        let Some(mut current_name) = path.next() else { return Some(self.clone().into()) };
        let mut current_dict = self.clone();
        loop {
            match path.next() {
                Some(next_name) => {
                    // Lifetimes are weird here with the MutexGuard, so we do this in two steps
                    let sub_dict = current_dict
                        .lock_entries()
                        .get(current_name.as_str())
                        .and_then(|value| value.clone().to_dictionary())?;
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => return current_dict.lock_entries().get(current_name.as_str()).cloned(),
            }
        }
    }

    /// Attempts to walk `path` in `self` until a router is found. If one is, it is then downscoped
    /// to the remaining path. If a router is not found, then a router is returned which will
    /// unconditionally fail routing requests with `error`.
    fn get_router_or_error<'a>(
        &self,
        mut path: impl DoubleEndedIterator<Item = &'a Name>,
        error: RoutingError,
    ) -> Router {
        let mut current = self.clone();
        while let Some(next_element) = path.next() {
            match current.get_capability(iter::once(next_element)) {
                Some(Capability::Dictionary(dictionary)) => current = dictionary,
                Some(Capability::Router(r)) => return Router::from_any(r).with_path(path),
                Some(cap) if path.next().is_none() => return Router::new(cap),
                _ => return Router::new_error(error),
            }
        }
        Router::new_error(error)
    }

    fn insert_capability<'a>(
        &self,
        mut path: impl Iterator<Item = &'a Name>,
        capability: Capability,
    ) {
        let mut current_name = path.next().expect("path must be non-empty");
        let mut current_dict = self.clone();
        loop {
            match path.next() {
                Some(next_name) => {
                    // Lifetimes are weird here with the MutexGuard, so we do this in two steps
                    let sub_dict = current_dict
                        .lock_entries()
                        .entry(current_name.clone())
                        .or_insert(Capability::Dictionary(Dict::new()))
                        .clone()
                        .to_dictionary()
                        .unwrap();
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => {
                    current_dict.lock_entries().insert(current_name.clone(), capability);
                    return;
                }
            }
        }
    }

    fn remove_capability<'a>(&self, mut path: impl Iterator<Item = &'a Name>) {
        let mut current_name = path.next().expect("path must be non-empty");
        let mut current_dict = self.clone();
        loop {
            match path.next() {
                Some(next_name) => {
                    let sub_dict = current_dict
                        .lock_entries()
                        .get(current_name.as_str())
                        .and_then(|value| value.clone().to_dictionary());
                    if sub_dict.is_none() {
                        // The capability doesn't exist, there's nothing to remove.
                        return;
                    }
                    current_dict = sub_dict.unwrap();
                    current_name = next_name;
                }
                None => {
                    current_dict.lock_entries().remove(current_name.as_str());
                    return;
                }
            }
        }
    }

    async fn get_with_request<'a>(
        &self,
        path: impl Iterator<Item = &'a Name>,
        request: Request,
    ) -> Result<Option<Capability>, BedrockError> {
        let mut current_capability: Capability = self.clone().into();
        for next_name in path {
            // We have another name but no subdictionary, so exit.
            let Capability::Dictionary(current_dict) = &current_capability else { return Ok(None) };

            // Get the capability.
            let capability = current_dict.lock_entries().get(next_name.as_str()).cloned();

            // The capability doesn't exist.
            let Some(capability) = capability else { return Ok(None) };

            // Resolve the capability, this is a noop if it's not a router.
            current_capability = capability.route(request.clone()).await?;
        }
        Ok(Some(current_capability))
    }
}

/// Waits for a new message on a receiver, and launches a new async task on a `WeakTaskGroup` to
/// handle each new message from the receiver.
pub struct LaunchTaskOnReceive {
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

impl std::fmt::Debug for LaunchTaskOnReceive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchTaskOnReceive").field("task_name", &self.task_name).finish()
    }
}

impl LaunchTaskOnReceive {
    pub fn new(
        task_group: WeakTaskGroup,
        task_name: impl Into<String>,
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
        Self { task_to_launch, task_group, policy, task_name: task_name.into() }
    }

    pub fn into_sender(self: Arc<Self>, target: WeakComponentInstance) -> Sender {
        #[derive(Debug)]
        struct TaskAndTarget {
            task: Arc<LaunchTaskOnReceive>,
            target: WeakComponentInstance,
        }

        impl Sendable for TaskAndTarget {
            fn send(&self, message: Message) -> Result<(), ()> {
                self.task.launch_task(message.channel, self.target.clone());
                Ok(())
            }
        }

        Sender::new_sendable(TaskAndTarget { task: self, target })
    }

    pub fn into_router(self) -> Router {
        Router::new(Arc::new(self))
    }

    fn launch_task(&self, channel: zx::Channel, instance: WeakComponentInstance) {
        if let Some((policy_checker, capability_source)) = &self.policy {
            if let Err(_e) =
                policy_checker.can_route_capability(&capability_source, &instance.moniker)
            {
                // The `can_route_capability` function above will log an error, so we don't
                // have to.
                let _ = channel.close_with_epitaph(zx::Status::ACCESS_DENIED);
                return;
            }
        }

        let fut = (self.task_to_launch)(channel, instance);
        let task_name = self.task_name.clone();
        self.task_group.spawn(async move {
            if let Err(error) = fut.await {
                warn!(%error, "{} failed", task_name);
            }
        });
    }

    // Create a new LaunchTaskOnReceive that represents a framework hook task.
    // The task that this launches finds the components internal provider and will
    // open that.
    pub fn new_hook_launch_task(
        component: &Arc<ComponentInstance>,
        capability_source: CapabilitySource<ComponentInstance>,
    ) -> LaunchTaskOnReceive {
        let weak_component = WeakComponentInstance::new(component);
        LaunchTaskOnReceive::new(
            component.nonblocking_task_group().as_weak(),
            "framework hook dispatcher",
            Some((component.context.policy().clone(), capability_source.clone())),
            Arc::new(move |mut channel, target| {
                let weak_component = weak_component.clone();
                let capability_source = capability_source.clone();
                async move {
                    if let Ok(target) = target.upgrade() {
                        if let Ok(component) = weak_component.upgrade() {
                            if let Some(provider) = target
                                .context
                                .find_internal_provider(&capability_source, target.as_weak())
                                .await
                            {
                                provider
                                    .open(
                                        component.nonblocking_task_group(),
                                        fio::OpenFlags::empty(),
                                        PathBuf::from(""),
                                        &mut channel,
                                    )
                                    .await?;
                                return Ok(());
                            }
                        }

                        let _ = channel.close_with_epitaph(zx::Status::UNAVAILABLE);
                    }
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[async_trait]
impl Routable for Arc<LaunchTaskOnReceive> {
    async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
        Ok(self.clone().into_sender(request.target).into())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use bedrock_error::DowncastErrorForTest;
    use cm_rust::Availability;
    use sandbox::{Data, Receiver};

    #[fuchsia::test]
    async fn get_capability() {
        let sub_dict = Dict::new();
        sub_dict.lock_entries().insert("bar".parse().unwrap(), Capability::Dictionary(Dict::new()));
        let (_, sender) = Receiver::new();
        sub_dict.lock_entries().insert("baz".parse().unwrap(), sender.into());

        let test_dict = Dict::new();
        test_dict.lock_entries().insert("foo".parse().unwrap(), Capability::Dictionary(sub_dict));

        assert!(test_dict.get_capability(iter::empty()).is_some());
        assert!(test_dict.get_capability(iter::once(&"nonexistent".parse().unwrap())).is_none());
        assert!(test_dict.get_capability(iter::once(&"foo".parse().unwrap())).is_some());
        assert!(test_dict
            .get_capability([&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter())
            .is_some());
        assert!(test_dict
            .get_capability([&"foo".parse().unwrap(), &"nonexistent".parse().unwrap()].into_iter())
            .is_none());
        assert!(test_dict
            .get_capability([&"foo".parse().unwrap(), &"baz".parse().unwrap()].into_iter())
            .is_some());
    }

    #[fuchsia::test]
    async fn insert_capability() {
        let test_dict = Dict::new();
        test_dict.insert_capability(
            [&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter(),
            Dict::new().into(),
        );
        assert!(test_dict
            .get_capability([&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter())
            .is_some());

        let (_, sender) = Receiver::new();
        test_dict.insert_capability(
            [&"foo".parse().unwrap(), &"baz".parse().unwrap()].into_iter(),
            sender.into(),
        );
        assert!(test_dict
            .get_capability([&"foo".parse().unwrap(), &"baz".parse().unwrap()].into_iter())
            .is_some());
    }

    #[fuchsia::test]
    async fn remove_capability() {
        let test_dict = Dict::new();
        test_dict.insert_capability(
            [&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter(),
            Dict::new().into(),
        );
        assert!(test_dict
            .get_capability([&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter())
            .is_some());

        test_dict.remove_capability([&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter());
        assert!(test_dict
            .get_capability([&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter())
            .is_none());
        assert!(test_dict.get_capability([&"foo".parse().unwrap()].into_iter()).is_some());

        test_dict.remove_capability(iter::once(&"foo".parse().unwrap()));
        assert!(test_dict.get_capability([&"foo".parse().unwrap()].into_iter()).is_none());
    }

    #[fuchsia::test]
    async fn get_with_request_ok() {
        let bar = Dict::new();
        let data = Data::String("hello".to_owned());
        bar.insert_capability(iter::once(&"data".parse().unwrap()), data.into());
        // Put bar behind a few layers of Router for good measure.
        let bar_router = Router::new_ok(bar);
        let bar_router = Router::new_ok(bar_router);
        let bar_router = Router::new_ok(bar_router);

        let foo = Dict::new();
        foo.insert_capability(iter::once(&"bar".parse().unwrap()), bar_router.into());
        let foo_router = Router::new_ok(foo);

        let dict = Dict::new();
        dict.insert_capability(iter::once(&"foo".parse().unwrap()), foo_router.into());

        let cap = dict
            .get_with_request(
                [&"foo".parse().unwrap(), &"bar".parse().unwrap(), &"data".parse().unwrap()]
                    .into_iter(),
                Request {
                    availability: Availability::Required,
                    target: WeakComponentInstance::invalid(),
                },
            )
            .await;
        assert_matches!(cap, Ok(Some(Capability::Data(Data::String(str)))) if str == "hello");
    }

    #[fuchsia::test]
    async fn get_with_request_error() {
        let dict = Dict::new();
        let foo = Router::new_error(RoutingError::SourceCapabilityIsVoid);
        dict.insert_capability(iter::once(&"foo".parse().unwrap()), foo.into());
        let cap = dict
            .get_with_request(
                [&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter(),
                Request {
                    availability: Availability::Required,
                    target: WeakComponentInstance::invalid(),
                },
            )
            .await;
        assert_matches!(
            cap,
            Err(BedrockError::RoutingError(err))
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::SourceCapabilityIsVoid
            )
        );
    }

    #[fuchsia::test]
    async fn get_with_request_missing() {
        let dict = Dict::new();
        let cap = dict
            .get_with_request(
                [&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter(),
                Request {
                    availability: Availability::Required,
                    target: WeakComponentInstance::invalid(),
                },
            )
            .await;
        assert_matches!(cap, Ok(None));
    }

    #[fuchsia::test]
    async fn get_with_request_missing_deep() {
        let dict = Dict::new();

        let foo = Dict::new();
        let foo = Router::new_ok(foo);
        dict.insert_capability(iter::once(&"foo".parse().unwrap()), foo.into());

        let cap = dict
            .get_with_request(
                [&"foo".parse().unwrap()].into_iter(),
                Request {
                    availability: Availability::Required,
                    target: WeakComponentInstance::invalid(),
                },
            )
            .await;
        assert_matches!(cap, Ok(Some(Capability::Dictionary(_))));

        let cap = dict
            .get_with_request(
                [&"foo".parse().unwrap(), &"bar".parse().unwrap()].into_iter(),
                Request {
                    availability: Availability::Required,
                    target: WeakComponentInstance::invalid(),
                },
            )
            .await;
        assert_matches!(cap, Ok(None));
    }
}
