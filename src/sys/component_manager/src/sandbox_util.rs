// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::{ComponentInstance, WeakComponentInstance},
        routing::router::{Request, Routable, Router},
    },
    ::routing::{
        capability_source::CapabilitySource, component_instance::ComponentInstanceInterface,
        error::RoutingError, policy::GlobalPolicyChecker,
    },
    async_trait::async_trait,
    bedrock_error::BedrockError,
    cm_types::IterablePath,
    cm_util::WeakTaskGroup,
    fidl::{
        endpoints::{ProtocolMarker, RequestStream},
        epitaph::ChannelEpitaphExt,
        AsyncChannel,
    },
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::{future::BoxFuture, FutureExt},
    sandbox::{Capability, Dict, Message, Open, Sendable, Sender},
    std::{fmt::Debug, sync::Arc},
    tracing::warn,
    vfs::{
        directory::entry::{DirectoryEntry, DirectoryEntryAsync, EntryInfo, OpenRequest},
        execution_scope::ExecutionScope,
        path::Path,
        ToObjectRequest,
    },
};

pub fn take_handle_as_stream<P: ProtocolMarker>(channel: zx::Channel) -> P::RequestStream {
    let channel = AsyncChannel::from_channel(channel);
    P::RequestStream::from_channel(channel)
}

pub trait DictExt {
    /// Returns the capability at the path, if it exists. Returns `None` if path is empty.
    fn get_capability(&self, path: &impl IterablePath) -> Option<Capability>;

    /// Inserts the capability at the path. Intermediary dictionaries are created as needed.
    fn insert_capability(
        &self,
        path: &impl IterablePath,
        capability: Capability,
    ) -> Result<(), fsandbox::DictionaryError>;

    /// Removes the capability at the path, if it exists.
    fn remove_capability(&self, path: &impl IterablePath);

    /// Looks up the element at `path`. When encountering an intermediate router, use `request`
    /// to request the underlying capability from it. In contrast, `get_capability` will return
    /// `None`.
    async fn get_with_request<'a>(
        &self,
        path: &'a impl IterablePath,
        request: Request,
    ) -> Result<Option<Capability>, BedrockError>;
}

impl DictExt for Dict {
    fn get_capability(&self, path: &impl IterablePath) -> Option<Capability> {
        let mut segments = path.iter_segments();
        let Some(mut current_name) = segments.next() else { return Some(self.clone().into()) };
        let mut current_dict = self.clone();
        loop {
            match segments.next() {
                Some(next_name) => {
                    // Lifetimes are weird here with the MutexGuard, so we do this in two steps
                    let sub_dict = current_dict
                        .lock_entries()
                        .get(current_name)
                        .and_then(|value| value.clone().to_dictionary())?;
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => return current_dict.lock_entries().get(current_name).cloned(),
            }
        }
    }

    fn insert_capability(
        &self,
        path: &impl IterablePath,
        capability: Capability,
    ) -> Result<(), fsandbox::DictionaryError> {
        let mut segments = path.iter_segments();
        let mut current_name = segments.next().expect("path must be non-empty");
        let mut current_dict = self.clone();
        loop {
            match segments.next() {
                Some(next_name) => {
                    // Lifetimes are weird here with the MutexGuard, so we do this in two steps
                    let sub_dict = {
                        let mut entries = current_dict.lock_entries();
                        match entries.get(current_name) {
                            Some(cap) => cap.clone().to_dictionary().unwrap(),
                            None => {
                                let cap = Capability::Dictionary(Dict::new());
                                entries.insert(current_name.clone(), cap.clone())?;
                                cap.to_dictionary().unwrap()
                            }
                        }
                    };
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => {
                    return current_dict.lock_entries().insert(current_name.clone(), capability);
                }
            }
        }
    }

    fn remove_capability(&self, path: &impl IterablePath) {
        let mut segments = path.iter_segments();
        let mut current_name = segments.next().expect("path must be non-empty");
        let mut current_dict = self.clone();
        loop {
            match segments.next() {
                Some(next_name) => {
                    let sub_dict = current_dict
                        .lock_entries()
                        .get(current_name)
                        .and_then(|value| value.clone().to_dictionary());
                    if sub_dict.is_none() {
                        // The capability doesn't exist, there's nothing to remove.
                        return;
                    }
                    current_dict = sub_dict.unwrap();
                    current_name = next_name;
                }
                None => {
                    current_dict.lock_entries().remove(current_name);
                    return;
                }
            }
        }
    }

    async fn get_with_request<'a>(
        &self,
        path: &'a impl IterablePath,
        request: Request,
    ) -> Result<Option<Capability>, BedrockError> {
        let mut current_capability: Capability = self.clone().into();
        for next_name in path.iter_segments() {
            // We have another name but no subdictionary, so exit.
            let Capability::Dictionary(current_dict) = &current_capability else { return Ok(None) };

            // Get the capability.
            let capability = current_dict.lock_entries().get(next_name).cloned();

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
            Arc::new(move |channel, target| {
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
                                let mut object_request =
                                    fio::OpenFlags::empty().to_object_request(channel);
                                provider
                                    .open(
                                        component.nonblocking_task_group(),
                                        OpenRequest::new(
                                            component.execution_scope.clone(),
                                            fio::OpenFlags::empty(),
                                            Path::dot(),
                                            &mut object_request,
                                        ),
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

/// Porcelain methods on [`Routable`] objects.
pub trait RoutableExt: Routable {
    /// Returns a router that resolves with a [`sandbox::Sender`] that watches for
    /// the channel to be readable, then delegates to the current router. The wait
    /// is performed in the provided `scope`.
    fn on_readable(self, scope: ExecutionScope, entry_type: fio::DirentType) -> Router;

    /// Returns a router that requests capabilities from the specified `path` relative to
    /// the base routable or fails the request with `not_found_error` if the member is not
    /// found. The base routable should resolve with a dictionary capability.
    fn lazy_get<P>(self, path: P, not_found_error: impl Into<BedrockError>) -> Router
    where
        P: IterablePath + Debug + 'static;
}

impl<T: Routable + 'static> RoutableExt for T {
    fn on_readable(self, scope: ExecutionScope, entry_type: fio::DirentType) -> Router {
        #[derive(Debug)]
        struct OnReadableRouter {
            router: Router,
            scope: ExecutionScope,
            entry_type: fio::DirentType,
        }

        #[async_trait]
        impl Routable for OnReadableRouter {
            async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
                let target = request.target.upgrade().map_err(RoutingError::from)?;
                let entry = self.router.clone().into_directory_entry(
                    request,
                    self.entry_type,
                    move |err| {
                        // TODO(https://fxbug.dev/319754472): Improve the fidelity of error logging.
                        // This should log into the component's log sink using the proper
                        // `report_routing_failure`, but that function requires a legacy
                        // `RouteRequest` at the moment.
                        let target = target.clone();
                        Some(Box::pin(async move {
                            target
                                .with_logger_as_default(|| {
                                    warn!(
                                        "Request was not available for target component `{}`: `{}`",
                                        target.moniker, err
                                    );
                                })
                                .await
                        }))
                    },
                );

                // Wrap the entry in something that will wait until the channel is readable.
                struct OnReadable(ExecutionScope, Arc<dyn DirectoryEntry>);

                impl DirectoryEntry for OnReadable {
                    fn entry_info(&self) -> EntryInfo {
                        self.1.entry_info()
                    }

                    fn open_entry(
                        self: Arc<Self>,
                        mut request: OpenRequest<'_>,
                    ) -> Result<(), zx::Status> {
                        request.set_scope(self.0.clone());
                        if request.path().is_empty() && !request.requires_event() {
                            request.spawn(self);
                            Ok(())
                        } else {
                            self.1.clone().open_entry(request)
                        }
                    }
                }

                impl DirectoryEntryAsync for OnReadable {
                    async fn open_entry_async(
                        self: Arc<Self>,
                        request: OpenRequest<'_>,
                    ) -> Result<(), zx::Status> {
                        if request.wait_till_ready().await {
                            self.1.clone().open_entry(request)
                        } else {
                            // The channel was closed.
                            Ok(())
                        }
                    }
                }

                Ok(Capability::Open(Open::new(
                    Arc::new(OnReadable(self.scope.clone(), entry)) as Arc<dyn DirectoryEntry>
                )))
            }
        }

        let router = Router::new(self);
        Router::new(OnReadableRouter { router, scope, entry_type })
    }

    fn lazy_get<P>(self, path: P, not_found_error: impl Into<BedrockError>) -> Router
    where
        P: IterablePath + Debug + 'static,
    {
        #[derive(Debug)]
        struct ScopedDictRouter<P: IterablePath + Debug + 'static> {
            router: Router,
            path: P,
            not_found_error: BedrockError,
        }

        #[async_trait]
        impl<P: IterablePath + Debug + 'static> Routable for ScopedDictRouter<P> {
            async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
                match self.router.route(request.clone()).await? {
                    Capability::Dictionary(dict) => {
                        let maybe_capability =
                            dict.get_with_request(&self.path, request.clone()).await?;
                        maybe_capability.ok_or_else(|| self.not_found_error.clone())
                    }
                    _ => Err(RoutingError::BedrockMemberAccessUnsupported.into()),
                }
            }
        }

        Router::new(ScopedDictRouter {
            router: Router::new(self),
            path,
            not_found_error: not_found_error.into(),
        })
    }
}

#[cfg(test)]
pub mod tests {
    use crate::model::{context::ModelContext, environment::Environment};

    use super::*;
    use assert_matches::assert_matches;
    use bedrock_error::DowncastErrorForTest;
    use cm_rust::Availability;
    use cm_types::RelativePath;
    use fuchsia_async::TestExecutor;
    use sandbox::{Data, Receiver};
    use std::{pin::pin, sync::Weak, task::Poll};

    #[fuchsia::test]
    async fn get_capability() {
        let sub_dict = Dict::new();
        sub_dict
            .lock_entries()
            .insert("bar".parse().unwrap(), Capability::Dictionary(Dict::new()))
            .expect("dict entry already exists");
        let (_, sender) = Receiver::new();
        sub_dict
            .lock_entries()
            .insert("baz".parse().unwrap(), sender.into())
            .expect("dict entry already exists");

        let test_dict = Dict::new();
        test_dict
            .lock_entries()
            .insert("foo".parse().unwrap(), Capability::Dictionary(sub_dict))
            .expect("dict entry already exists");

        assert!(test_dict.get_capability(&RelativePath::dot()).is_some());
        assert!(test_dict.get_capability(&RelativePath::new("nonexistent").unwrap()).is_none());
        assert!(test_dict.get_capability(&RelativePath::new("foo").unwrap()).is_some());
        assert!(test_dict.get_capability(&RelativePath::new("foo/bar").unwrap()).is_some());
        assert!(test_dict.get_capability(&RelativePath::new("foo/nonexistent").unwrap()).is_none());
        assert!(test_dict.get_capability(&RelativePath::new("foo/baz").unwrap()).is_some());
    }

    #[fuchsia::test]
    async fn insert_capability() {
        let test_dict = Dict::new();
        assert!(test_dict
            .insert_capability(&RelativePath::new("foo/bar").unwrap(), Dict::new().into())
            .is_ok());
        assert!(test_dict.get_capability(&RelativePath::new("foo/bar").unwrap()).is_some());

        let (_, sender) = Receiver::new();
        assert!(test_dict
            .insert_capability(&RelativePath::new("foo/baz").unwrap(), sender.into())
            .is_ok());
        assert!(test_dict.get_capability(&RelativePath::new("foo/baz").unwrap()).is_some());
    }

    #[fuchsia::test]
    async fn remove_capability() {
        let test_dict = Dict::new();
        assert!(test_dict
            .insert_capability(&RelativePath::new("foo/bar").unwrap(), Dict::new().into())
            .is_ok());
        assert!(test_dict.get_capability(&RelativePath::new("foo/bar").unwrap()).is_some());

        test_dict.remove_capability(&RelativePath::new("foo/bar").unwrap());
        assert!(test_dict.get_capability(&RelativePath::new("foo/bar").unwrap()).is_none());
        assert!(test_dict.get_capability(&RelativePath::new("foo").unwrap()).is_some());

        test_dict.remove_capability(&RelativePath::new("foo").unwrap());
        assert!(test_dict.get_capability(&RelativePath::new("foo").unwrap()).is_none());
    }

    #[fuchsia::test]
    async fn get_with_request_ok() {
        let bar = Dict::new();
        let data = Data::String("hello".to_owned());
        assert!(bar.insert_capability(&RelativePath::new("data").unwrap(), data.into()).is_ok());
        // Put bar behind a few layers of Router for good measure.
        let bar_router = Router::new_ok(bar);
        let bar_router = Router::new_ok(bar_router);
        let bar_router = Router::new_ok(bar_router);

        let foo = Dict::new();
        assert!(foo
            .insert_capability(&RelativePath::new("bar").unwrap(), bar_router.into())
            .is_ok());
        let foo_router = Router::new_ok(foo);

        let dict = Dict::new();
        assert!(dict
            .insert_capability(&RelativePath::new("foo").unwrap(), foo_router.into())
            .is_ok());

        let cap = dict
            .get_with_request(
                &RelativePath::new("foo/bar/data").unwrap(),
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
        assert!(dict.insert_capability(&RelativePath::new("foo").unwrap(), foo.into()).is_ok());
        let cap = dict
            .get_with_request(
                &RelativePath::new("foo/bar").unwrap(),
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
                &RelativePath::new("foo/bar").unwrap(),
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
        assert!(dict.insert_capability(&RelativePath::new("foo").unwrap(), foo.into()).is_ok());

        let cap = dict
            .get_with_request(
                &RelativePath::new("foo").unwrap(),
                Request {
                    availability: Availability::Required,
                    target: WeakComponentInstance::invalid(),
                },
            )
            .await;
        assert_matches!(cap, Ok(Some(Capability::Dictionary(_))));

        let cap = dict
            .get_with_request(
                &RelativePath::new("foo/bar").unwrap(),
                Request {
                    availability: Availability::Required,
                    target: WeakComponentInstance::invalid(),
                },
            )
            .await;
        assert_matches!(cap, Ok(None));
    }

    #[derive(Debug, Clone)]
    struct RouteCounter {
        capability: Capability,
        counter: Arc<test_util::Counter>,
    }

    impl RouteCounter {
        fn new(capability: Capability) -> Self {
            Self { capability, counter: Arc::new(test_util::Counter::new(0)) }
        }

        fn count(&self) -> usize {
            self.counter.get()
        }
    }

    #[async_trait]
    impl Routable for RouteCounter {
        async fn route(&self, _: Request) -> Result<Capability, BedrockError> {
            self.counter.inc();
            Ok(self.capability.clone())
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn router_on_readable_client_writes() {
        let (receiver, sender) = Receiver::new();
        let scope = ExecutionScope::new();
        let (client_end, server_end) = zx::Channel::create();

        let route_counter = RouteCounter::new(sender.into());
        let router = route_counter.clone().on_readable(scope.clone(), fio::DirentType::Service);

        let mut receive = pin!(receiver.receive());
        assert_matches!(TestExecutor::poll_until_stalled(&mut receive).await, Poll::Pending);

        let component = ComponentInstance::new_root(
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "test:///root".to_string(),
        )
        .await;
        let capability = router
            .route(Request { availability: Availability::Required, target: component.as_weak() })
            .await
            .unwrap();

        assert_matches!(TestExecutor::poll_until_stalled(&mut receive).await, Poll::Pending);
        assert_eq!(route_counter.count(), 0);

        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        capability
            .try_into_directory_entry()
            .unwrap()
            .open_entry(OpenRequest::new(
                scope.clone(),
                fio::OpenFlags::empty(),
                Path::dot(),
                &mut object_request,
            ))
            .unwrap();

        assert_matches!(TestExecutor::poll_until_stalled(&mut receive).await, Poll::Pending);
        assert_eq!(route_counter.count(), 0);

        client_end.write(&[0], &mut []).unwrap();
        assert_matches!(TestExecutor::poll_until_stalled(&mut receive).await, Poll::Ready(Some(_)));
        scope.wait().await;
        assert_eq!(route_counter.count(), 1);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn router_on_readable_client_closes() {
        let (receiver, sender) = Receiver::new();
        let scope = ExecutionScope::new();
        let (client_end, server_end) = zx::Channel::create();

        let route_counter = RouteCounter::new(sender.into());
        let router = route_counter.clone().on_readable(scope.clone(), fio::DirentType::Service);

        let mut receive = pin!(receiver.receive());
        assert_matches!(TestExecutor::poll_until_stalled(&mut receive).await, Poll::Pending);

        let component = ComponentInstance::new_root(
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "test:///root".to_string(),
        )
        .await;
        let capability = router
            .route(Request { availability: Availability::Required, target: component.as_weak() })
            .await
            .unwrap();

        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        capability
            .try_into_directory_entry()
            .unwrap()
            .open_entry(OpenRequest::new(
                scope.clone(),
                fio::OpenFlags::empty(),
                Path::dot(),
                &mut object_request,
            ))
            .unwrap();

        assert_matches!(TestExecutor::poll_until_stalled(&mut receive).await, Poll::Pending);
        assert_matches!(
            TestExecutor::poll_until_stalled(Box::pin(scope.clone().wait())).await,
            Poll::Pending
        );
        assert_eq!(route_counter.count(), 0);

        drop(client_end);
        assert_matches!(TestExecutor::poll_until_stalled(&mut receive).await, Poll::Pending);
        scope.wait().await;
        assert_eq!(route_counter.count(), 0);
    }

    #[fuchsia::test]
    async fn lazy_get() {
        let source = Capability::Data(Data::String("hello".to_string()));
        let dict1 = Dict::new();
        dict1
            .lock_entries()
            .insert("source".parse().unwrap(), source)
            .expect("dict entry already exists");

        let base_router = Router::new_ok(dict1);
        let downscoped_router = base_router.lazy_get(
            RelativePath::new("source").unwrap(),
            RoutingError::BedrockMemberAccessUnsupported,
        );

        let capability = downscoped_router
            .route(Request {
                availability: Availability::Optional,
                target: WeakComponentInstance::invalid(),
            })
            .await
            .unwrap();
        let capability = match capability {
            Capability::Data(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }

    #[fuchsia::test]
    async fn lazy_get_deep() {
        let source = Capability::Data(Data::String("hello".to_string()));
        let dict1 = Dict::new();
        dict1
            .lock_entries()
            .insert("source".parse().unwrap(), source)
            .expect("dict entry already exists");
        let dict2 = Dict::new();
        dict2
            .lock_entries()
            .insert("dict1".parse().unwrap(), Capability::Dictionary(dict1))
            .expect("dict entry already exists");
        let dict3 = Dict::new();
        dict3
            .lock_entries()
            .insert("dict2".parse().unwrap(), Capability::Dictionary(dict2))
            .expect("dict entry already exists");
        let dict4 = Dict::new();
        dict4
            .lock_entries()
            .insert("dict3".parse().unwrap(), Capability::Dictionary(dict3))
            .expect("dict entry already exists");

        let base_router = Router::new_ok(dict4);
        let downscoped_router = base_router.lazy_get(
            RelativePath::new("dict3/dict2/dict1/source").unwrap(),
            RoutingError::BedrockMemberAccessUnsupported,
        );

        let capability = downscoped_router
            .route(Request {
                availability: Availability::Optional,
                target: WeakComponentInstance::invalid(),
            })
            .await
            .unwrap();
        let capability = match capability {
            Capability::Data(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }
}
