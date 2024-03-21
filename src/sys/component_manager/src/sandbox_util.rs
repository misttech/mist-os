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
    sandbox::{Capability, Dict, Open},
    std::iter,
    std::sync::Arc,
    tracing::warn,
    vfs::{execution_scope::ExecutionScope, service::endpoint},
};

pub fn take_handle_as_stream<P: ProtocolMarker>(channel: zx::Channel) -> P::RequestStream {
    let channel = AsyncChannel::from_channel(channel);
    P::RequestStream::from_channel(channel)
}

// TODO: use the `Name` type in `Dict`, so that Dicts aren't holding duplicate strings.

pub trait DictExt {
    fn get_or_insert_sub_dict<'a>(&self, path: impl Iterator<Item = &'a str>) -> Dict;

    /// Returns the capability at the path, if it exists. Returns `None` if path is empty.
    fn get_capability<'a>(&self, path: impl Iterator<Item = &'a str>) -> Option<Capability>;

    /// Attempts to walk `path` in `self` until a router is found. If one is, it is then downscoped
    /// to the remaining path. If a router is not found, then a router is returned which will
    /// unconditionally fail routing requests with `error`.
    fn get_router_or_error<'a>(
        &self,
        path: impl DoubleEndedIterator<Item = &'a str>,
        error: RoutingError,
    ) -> Router;

    /// Inserts the capability at the path. Intermediary dictionaries are created as needed.
    fn insert_capability<'a>(&self, path: impl Iterator<Item = &'a str>, capability: Capability);

    /// Removes the capability at the path, if it exists.
    fn remove_capability<'a>(&self, path: impl Iterator<Item = &'a str>);

    /// Looks up the element at `path`. When encountering an intermediate router, use `request`
    /// to request the underlying capability from it. In contrast, `get_capability` will return
    /// `None`.
    async fn get_with_request<'a>(
        &self,
        path: impl Iterator<Item = &'a str>,
        request: Request,
    ) -> Result<Option<Capability>, BedrockError>;
}

impl DictExt for Dict {
    fn get_or_insert_sub_dict<'a>(&self, mut path: impl Iterator<Item = &'a str>) -> Dict {
        let Some(next_name) = path.next() else { return self.clone() };
        let sub_dict: Dict = self
            .lock_entries()
            .entry(next_name.to_string())
            .or_insert(Capability::Dictionary(Dict::new()))
            .clone()
            .to_dictionary()
            .unwrap();
        sub_dict.get_or_insert_sub_dict(path)
    }

    fn get_capability<'a>(&self, mut path: impl Iterator<Item = &'a str>) -> Option<Capability> {
        let Some(mut current_name) = path.next() else { return Some(self.clone().into()) };
        let mut current_dict = self.clone();
        loop {
            match path.next() {
                Some(next_name) => {
                    // Lifetimes are weird here with the MutexGuard, so we do this in two steps
                    let sub_dict = current_dict
                        .lock_entries()
                        .get(&current_name.to_string())
                        .and_then(|value| value.clone().to_dictionary())?;
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => return current_dict.lock_entries().get(&current_name.to_string()).cloned(),
            }
        }
    }

    /// Attempts to walk `path` in `self` until a router is found. If one is, it is then downscoped
    /// to the remaining path. If a router is not found, then a router is returned which will
    /// unconditionally fail routing requests with `error`.
    fn get_router_or_error<'a>(
        &self,
        mut path: impl DoubleEndedIterator<Item = &'a str>,
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
        mut path: impl Iterator<Item = &'a str>,
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
                        .entry(current_name.to_string())
                        .or_insert(Capability::Dictionary(Dict::new()))
                        .clone()
                        .to_dictionary()
                        .unwrap();
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => {
                    current_dict.lock_entries().insert(current_name.to_string(), capability);
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
                        .and_then(|value| value.clone().to_dictionary());
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

    async fn get_with_request<'a>(
        &self,
        path: impl Iterator<Item = &'a str>,
        request: Request,
    ) -> Result<Option<Capability>, BedrockError> {
        let mut current_capability: Capability = self.clone().into();
        for next_name in path {
            // We have another name but no subdictionary, so exit.
            let Capability::Dictionary(current_dict) = &current_capability else { return Ok(None) };

            // Get the capability.
            let capability = current_dict.lock_entries().get(&next_name.to_string()).cloned();

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

    pub fn into_open(self: Arc<Self>, target: WeakComponentInstance) -> Open {
        Open::new(endpoint(move |_scope: ExecutionScope, server_end: AsyncChannel| {
            self.launch_task(server_end.into_zx_channel(), target.clone());
        }))
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
        Ok(self.clone().into_open(request.target).into())
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
        sub_dict.lock_entries().insert("bar".to_string(), Capability::Dictionary(Dict::new()));
        let (_, sender) = Receiver::new();
        sub_dict.lock_entries().insert("baz".to_string(), sender.into());

        let test_dict = Dict::new();
        test_dict.lock_entries().insert("foo".to_string(), Capability::Dictionary(sub_dict));

        assert!(test_dict.get_capability(iter::empty()).is_some());
        assert!(test_dict.get_capability(iter::once("nonexistent")).is_none());
        assert!(test_dict.get_capability(iter::once("foo")).is_some());
        assert!(test_dict.get_capability(["foo", "bar"].into_iter()).is_some());
        assert!(test_dict.get_capability(["foo", "nonexistent"].into_iter()).is_none());
        assert!(test_dict.get_capability(["foo", "baz"].into_iter()).is_some());
    }

    #[fuchsia::test]
    async fn insert_capability() {
        let test_dict = Dict::new();
        test_dict.insert_capability(["foo", "bar"].into_iter(), Dict::new().into());
        assert!(test_dict.get_capability(["foo", "bar"].into_iter()).is_some());

        let (_, sender) = Receiver::new();
        test_dict.insert_capability(["foo", "baz"].into_iter(), sender.into());
        assert!(test_dict.get_capability(["foo", "baz"].into_iter()).is_some());
    }

    #[fuchsia::test]
    async fn remove_capability() {
        let test_dict = Dict::new();
        test_dict.insert_capability(["foo", "bar"].into_iter(), Dict::new().into());
        assert!(test_dict.get_capability(["foo", "bar"].into_iter()).is_some());

        test_dict.remove_capability(["foo", "bar"].into_iter());
        assert!(test_dict.get_capability(["foo", "bar"].into_iter()).is_none());
        assert!(test_dict.get_capability(["foo"].into_iter()).is_some());

        test_dict.remove_capability(iter::once("foo"));
        assert!(test_dict.get_capability(["foo"].into_iter()).is_none());
    }

    #[fuchsia::test]
    async fn get_with_request_ok() {
        let bar = Dict::new();
        let data = Data::String("hello".to_owned());
        bar.insert_capability(iter::once("data"), data.into());
        // Put bar behind a few layers of Router for good measure.
        let bar_router = Router::new_ok(bar);
        let bar_router = Router::new_ok(bar_router);
        let bar_router = Router::new_ok(bar_router);

        let foo = Dict::new();
        foo.insert_capability(iter::once("bar"), bar_router.into());
        let foo_router = Router::new_ok(foo);

        let dict = Dict::new();
        dict.insert_capability(iter::once("foo"), foo_router.into());

        let cap = dict
            .get_with_request(
                ["foo", "bar", "data"].into_iter(),
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
        dict.insert_capability(iter::once("foo"), foo.into());
        let cap = dict
            .get_with_request(
                ["foo", "bar"].into_iter(),
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
                ["foo", "bar"].into_iter(),
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
        dict.insert_capability(iter::once("foo"), foo.into());

        let cap = dict
            .get_with_request(
                ["foo"].into_iter(),
                Request {
                    availability: Availability::Required,
                    target: WeakComponentInstance::invalid(),
                },
            )
            .await;
        assert_matches!(cap, Ok(Some(Capability::Dictionary(_))));

        let cap = dict
            .get_with_request(
                ["foo", "bar"].into_iter(),
                Request {
                    availability: Availability::Required,
                    target: WeakComponentInstance::invalid(),
                },
            )
            .await;
        assert_matches!(cap, Ok(None));
    }
}
