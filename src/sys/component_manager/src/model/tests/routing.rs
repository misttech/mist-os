// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Tests of capability routing in ComponentManager.
///
/// Most routing tests should be defined as methods on the ::routing_test_helpers::CommonRoutingTest
/// type and should be run both in this file (using a CommonRoutingTest<RoutingTestBuilder>) and in
/// the cm_fidl_analyzer_tests crate (using a specialization of CommonRoutingTest for the static
/// routing analyzer). This ensures that the static analyzer's routing verification is consistent
/// with ComponentManager's intended routing behavior.
///
/// However, tests of behavior that is out-of-scope for the static analyzer (e.g. routing to/from
/// dynamic component instances) should be defined here.
use {
    crate::{
        capability::{CapabilityProvider, CapabilitySource},
        framework::realm::SDK_REALM_SERVICE,
        model::{
            actions::{ActionSet, DestroyAction, DestroyChildAction, ShutdownAction},
            component::StartReason,
            error::{
                CapabilityProviderError, ModelError, ResolveActionError,
                RouteAndOpenCapabilityError, StartActionError,
            },
            hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
            routing::{Route, RouteRequest, RouteSource, RoutingError},
            testing::{routing_test_helpers::*, test_helpers::*},
        },
    },
    ::routing::{
        capability_source::{AggregateCapability, ComponentCapability, InternalCapability},
        error::ComponentInstanceError,
        resolving::ResolverError,
    },
    anyhow::Error,
    assert_matches::assert_matches,
    async_trait::async_trait,
    cm_rust::*,
    cm_rust_testing::*,
    cm_task_scope::TaskScope,
    cm_util::channel,
    fidl::endpoints::ServerEnd,
    fidl_fidl_examples_routing_echo::{self as echo},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_resolution as fresolution, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_io as fio, fidl_fuchsia_mem as fmem, fuchsia_async as fasync,
    fuchsia_zircon as zx,
    futures::{channel::oneshot, join, lock::Mutex, StreamExt, TryStreamExt},
    maplit::btreemap,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ChildMoniker, ChildMonikerBase},
    routing_test_helpers::{
        default_service_capability, instantiate_common_routing_tests, RoutingTestModel,
    },
    std::{
        collections::HashSet,
        convert::{TryFrom, TryInto},
        path::PathBuf,
        sync::{Arc, Weak},
    },
    tracing::warn,
    vfs::pseudo_directory,
};

instantiate_common_routing_tests! { RoutingTestBuilder }

///   a
///    \
///     b
///
/// b: uses framework service /svc/fuchsia.component.Realm
#[fuchsia::test]
async fn use_framework_service() {
    pub struct MockRealmCapabilityProvider {
        scope_moniker: AbsoluteMoniker,
        host: MockRealmCapabilityHost,
    }

    impl MockRealmCapabilityProvider {
        pub fn new(scope_moniker: AbsoluteMoniker, host: MockRealmCapabilityHost) -> Self {
            Self { scope_moniker, host }
        }
    }

    #[async_trait]
    impl CapabilityProvider for MockRealmCapabilityProvider {
        async fn open(
            self: Box<Self>,
            task_scope: TaskScope,
            _flags: fio::OpenFlags,
            _relative_path: PathBuf,
            server_end: &mut zx::Channel,
        ) -> Result<(), CapabilityProviderError> {
            let server_end = channel::take_channel(server_end);
            let stream = ServerEnd::<fcomponent::RealmMarker>::new(server_end)
                .into_stream()
                .expect("could not convert channel into stream");
            let scope_moniker = self.scope_moniker.clone();
            let host = self.host.clone();
            task_scope
                .add_task(async move {
                    if let Err(e) = host.serve(scope_moniker, stream).await {
                        // TODO: Set an epitaph to indicate this was an unexpected error.
                        warn!("serve_realm failed: {}", e);
                    }
                })
                .await;
            Ok(())
        }
    }

    #[async_trait]
    impl Hook for MockRealmCapabilityHost {
        async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
            if let EventPayload::CapabilityRouted {
                source: CapabilitySource::Framework { capability, component },
                capability_provider,
            } = &event.payload
            {
                let mut capability_provider = capability_provider.lock().await;
                *capability_provider = self
                    .on_scoped_framework_capability_routed_async(
                        component.abs_moniker.clone(),
                        &capability,
                        capability_provider.take(),
                    )
                    .await?;
            }
            Ok(())
        }
    }

    #[derive(Clone)]
    pub struct MockRealmCapabilityHost {
        /// List of calls to `OpenExposedDir` with component's relative moniker.
        open_calls: Arc<Mutex<Vec<String>>>,
    }

    impl MockRealmCapabilityHost {
        pub fn new() -> Self {
            Self { open_calls: Arc::new(Mutex::new(vec![])) }
        }

        pub fn open_calls(&self) -> Arc<Mutex<Vec<String>>> {
            self.open_calls.clone()
        }

        async fn serve(
            &self,
            scope_moniker: AbsoluteMoniker,
            mut stream: fcomponent::RealmRequestStream,
        ) -> Result<(), Error> {
            while let Some(request) = stream.try_next().await? {
                match request {
                    fcomponent::RealmRequest::OpenExposedDir { responder, .. } => {
                        self.open_calls.lock().await.push(
                            scope_moniker
                                .path()
                                .last()
                                .expect("did not expect root component")
                                .name()
                                .to_string(),
                        );
                        responder.send(&mut Ok(()))?;
                    }
                    _ => {}
                }
            }
            Ok(())
        }

        pub async fn on_scoped_framework_capability_routed_async<'a>(
            &'a self,
            scope_moniker: AbsoluteMoniker,
            capability: &'a InternalCapability,
            capability_provider: Option<Box<dyn CapabilityProvider>>,
        ) -> Result<Option<Box<dyn CapabilityProvider>>, ModelError> {
            // If some other capability has already been installed, then there's nothing to
            // do here.
            if capability.matches_protocol(&SDK_REALM_SERVICE) {
                Ok(Some(Box::new(MockRealmCapabilityProvider::new(
                    scope_moniker.clone(),
                    self.clone(),
                )) as Box<dyn CapabilityProvider>))
            } else {
                Ok(capability_provider)
            }
        }
    }

    let components = vec![
        ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Framework,
                    source_name: "fuchsia.component.Realm".into(),
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    // RoutingTest installs the real RealmCapabilityHost. Installing the
    // MockRealmCapabilityHost here overrides the previously installed one.
    let realm_service_host = Arc::new(MockRealmCapabilityHost::new());
    test.model
        .root()
        .hooks
        .install(vec![HooksRegistration::new(
            "MockRealmCapabilityHost",
            vec![EventType::CapabilityRouted],
            Arc::downgrade(&realm_service_host) as Weak<dyn Hook>,
        )])
        .await;
    test.check_use_realm(vec!["b"].try_into().unwrap(), realm_service_host.open_calls()).await;
}

///   a
///    \
///     b
///
/// a: offers service /svc/foo from self as /svc/bar
/// b: uses service /svc/bar as /svc/hippo
///
/// This test verifies that the parent, if subscribed to the CapabilityRequested event will receive
/// if when the child connects to /svc/hippo.
#[fuchsia::test]
async fn capability_requested_event_at_parent() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".into(),
                    target_name: "bar_svc".into(),
                    target: OfferTarget::static_child("b".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::EventStream(UseEventStreamDecl {
                    source: UseSource::Parent,
                    source_name: "capability_requested".into(),
                    scope: None,
                    target_path: CapabilityPath::try_from("/events/capability_requested").unwrap(),
                    filter: Some(btreemap! {
                        "name".to_string() => DictionaryValue::Str("foo_svc".to_string())
                    }),
                    availability: Availability::Required,
                }))
                .add_lazy_child("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "bar_svc".into(),
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTestBuilder::new("a", components)
        .set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "capability_requested".into(),
        })])
        .build()
        .await;

    let namespace_root = test.bind_and_get_namespace(AbsoluteMoniker::root()).await;
    let event_stream =
        capability_util::connect_to_svc_in_namespace::<fcomponent::EventStreamMarker>(
            &namespace_root,
            &"/events/capability_requested".try_into().unwrap(),
        )
        .await;

    let namespace_b = test.bind_and_get_namespace(vec!["b"].try_into().unwrap()).await;
    let _echo_proxy = capability_util::connect_to_svc_in_namespace::<echo::EchoMarker>(
        &namespace_b,
        &"/svc/hippo".try_into().unwrap(),
    )
    .await;

    let event = event_stream.get_next().await.unwrap().into_iter().next().unwrap();

    // 'b' is the target and 'a' is receiving the event so the relative moniker
    // is './b'.
    assert_matches!(&event,
        fcomponent::Event {
            header: Some(fcomponent::EventHeader {
            moniker: Some(moniker), .. }), ..
        } if *moniker == "./b".to_string() );

    assert_matches!(&event,
        fcomponent::Event {
            header: Some(fcomponent::EventHeader {
            component_url: Some(component_url), .. }), ..
        } if *component_url == "test:///b".to_string() );

    assert_matches!(&event,
        fcomponent::Event {
            payload:
                    Some(fcomponent::EventPayload::CapabilityRequested(
                        fcomponent::CapabilityRequestedPayload { name: Some(name), .. })), ..}

    if *name == "foo_svc".to_string()
    );
}

///   a
///    \
///     b
///    / \
///  [c] [d]
/// a: offers service /svc/hippo to b
/// b: offers service /svc/hippo to collection, creates [c]
/// [c]: instance in collection uses service /svc/hippo
/// [d]: ditto, but with /data/hippo
#[fuchsia::test]
async fn use_in_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .directory(DirectoryDeclBuilder::new("foo_data").build())
                .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source_name: "foo_data".into(),
                    source: OfferSource::Self_,
                    target_name: "hippo_data".into(),
                    target: OfferTarget::static_child("b".to_string()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "foo_svc".into(),
                    source: OfferSource::Self_,
                    target_name: "hippo_svc".into(),
                    target: OfferTarget::static_child("b".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_lazy_child("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Framework,
                    source_name: "fuchsia.component.Realm".into(),
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source_name: "hippo_data".into(),
                    source: OfferSource::Parent,
                    target_name: "hippo_data".into(),
                    target: OfferTarget::Collection("coll".to_string()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: OfferSource::Parent,
                    target_name: "hippo_svc".into(),
                    target: OfferTarget::Collection("coll".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_transient_collection("coll")
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Directory(UseDirectoryDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "hippo_data".into(),
                    target_path: CapabilityPath::try_from("/data/hippo").unwrap(),
                    rights: fio::R_STAR_DIR,
                    subdir: None,
                    availability: Availability::Required,
                }))
                .build(),
        ),
        (
            "d",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "hippo_svc".into(),
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    test.create_dynamic_child(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "d".to_string(),
            url: "test:///d".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.check_use(
        vec!["b", "coll:c"].try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Ok),
    )
    .await;
    test.check_use(
        vec!["b", "coll:d"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

///   a
///    \
///     b
///      \
///      [c]
/// a: offers service /svc/hippo to b
/// b: creates [c]
/// [c]: tries to use /svc/hippo, but can't because service was not offered to its collection
#[fuchsia::test]
async fn use_in_collection_not_offered() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .directory(DirectoryDeclBuilder::new("foo_data").build())
                .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source_name: "foo_data".into(),
                    source: OfferSource::Self_,
                    target_name: "hippo_data".into(),
                    target: OfferTarget::static_child("b".to_string()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "foo_svc".into(),
                    source: OfferSource::Self_,
                    target_name: "hippo_svc".into(),
                    target: OfferTarget::static_child("b".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_lazy_child("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Framework,
                    source_name: "fuchsia.component.Realm".into(),
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_transient_collection("coll")
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Directory(UseDirectoryDecl {
                    source: UseSource::Parent,
                    source_name: "hippo_data".into(),
                    target_path: CapabilityPath::try_from("/data/hippo").unwrap(),
                    rights: fio::R_STAR_DIR,
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "hippo_svc".into(),
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    test.create_dynamic_child(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.check_use(
        vec!["b", "coll:c"].try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Err(zx::Status::UNAVAILABLE)),
    )
    .await;
    test.check_use(
        vec!["b", "coll:c"].try_into().unwrap(),
        CheckUse::Protocol {
            path: default_service_capability(),
            expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
        },
    )
    .await;
}

///   a
///    \
///     b
///    / \
///  [c] [d]
/// a: offers service /svc/hippo to b
/// b: creates [c] and [d], dynamically offers service /svc/hippo to [c], but not [d].
/// [c]: instance in collection uses service /svc/hippo
/// [d]: instance in collection tries and fails to use service /svc/hippo
#[fuchsia::test]
async fn dynamic_offer_from_parent() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "foo_svc".into(),
                    source: OfferSource::Self_,
                    target_name: "hippo_svc".into(),
                    target: OfferTarget::static_child("b".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_lazy_child("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "fuchsia.component.Realm".into(),
                    source: UseSource::Framework,
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_collection(
                    CollectionDeclBuilder::new_transient_collection("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic)
                        .build(),
                )
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: UseSource::Parent,
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        ),
        (
            "d",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: UseSource::Parent,
                    dependency_type: DependencyType::Strong,
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    test.create_dynamic_child_with_args(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source_name: Some("hippo_svc".to_string()),
                source: Some(fdecl::Ref::Parent(fdecl::ParentRef)),
                target_name: Some("hippo_svc".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.create_dynamic_child(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "d".to_string(),
            url: "test:///d".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.check_use(
        vec!["b", "coll:c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
    test.check_use(
        vec!["b", "coll:d"].try_into().unwrap(),
        CheckUse::Protocol {
            path: default_service_capability(),
            expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
        },
    )
    .await;
}

///    a
///   / \
/// [b] [c]
/// a: creates [b]. creates [c] in the same collection, with a dynamic offer from [b].
/// [b]: instance in collection exposes /svc/hippo
/// [c]: instance in collection uses /svc/hippo
#[fuchsia::test]
async fn dynamic_offer_siblings_same_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "fuchsia.component.Realm".into(),
                    source: UseSource::Framework,
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_collection(
                    CollectionDeclBuilder::new_transient_collection("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic)
                        .build(),
                )
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("hippo_svc").build())
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: ExposeSource::Self_,
                    target_name: "hippo_svc".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: UseSource::Parent,
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child_with_args(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source_name: Some("hippo_svc".to_string()),
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: Some("coll".to_string()),
                })),
                target_name: Some("hippo_svc".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

///    a
///   / \
/// [b] [c]
/// a: creates [b]. creates [c] in a different collection, with a dynamic offer from [b].
/// [b]: instance in `source_coll` exposes /svc/hippo
/// [c]: instance in `target_coll` uses /svc/hippo
#[fuchsia::test]
async fn dynamic_offer_siblings_cross_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "fuchsia.component.Realm".into(),
                    source: UseSource::Framework,
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_collection(
                    CollectionDeclBuilder::new_transient_collection("source_coll").build(),
                )
                .add_collection(
                    CollectionDeclBuilder::new_transient_collection("target_coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic)
                        .build(),
                )
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("hippo_svc").build())
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: ExposeSource::Self_,
                    target_name: "hippo_svc".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: UseSource::Parent,
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "source_coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child_with_args(
        &AbsoluteMoniker::root(),
        "target_coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: Some("source_coll".to_string()),
                })),
                source_name: Some("hippo_svc".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                target_name: Some("hippo_svc".to_string()),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["target_coll:c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

///    a
///   / \
/// [b] [c]
/// a: creates [b]. creates [c] in the same collection, with a dynamic offer from [b].
/// [b]: instance in collection exposes /svc/hippo
/// [c]: instance in collection uses /svc/hippo. Can't use it after [b] is destroyed and recreated.
#[fuchsia::test]
async fn dynamic_offer_destroyed_on_source_destruction() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "fuchsia.component.Realm".into(),
                    source: UseSource::Framework,
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_collection(
                    CollectionDeclBuilder::new_transient_collection("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic)
                        .build(),
                )
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("hippo_svc").build())
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: ExposeSource::Self_,
                    target_name: "hippo_svc".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: UseSource::Parent,
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child_with_args(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source_name: Some("hippo_svc".to_string()),
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: Some("coll".to_string()),
                })),
                target_name: Some("hippo_svc".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;

    test.destroy_dynamic_child(AbsoluteMoniker::root(), "coll", "b").await;
    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;

    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::Protocol {
            path: default_service_capability(),
            expected_res: ExpectedResult::Err(zx::Status::UNAVAILABLE),
        },
    )
    .await;
}

///    a
///   / \
/// [b] [c]
/// a: creates [b]. creates [c] in the same collection, with a dynamic offer from [b].
/// [b]: instance in collection exposes /data/hippo
/// [c]: instance in collection uses /data/hippo. Can't use it after [c] is destroyed and recreated.
#[fuchsia::test]
async fn dynamic_offer_destroyed_on_target_destruction() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "fuchsia.component.Realm".into(),
                    source: UseSource::Framework,
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_collection(
                    CollectionDeclBuilder::new_transient_collection("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic)
                        .build(),
                )
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .directory(DirectoryDeclBuilder::new("hippo_data").build())
                .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                    source_name: "hippo_data".into(),
                    source: ExposeSource::Self_,
                    target_name: "hippo_data".into(),
                    target: ExposeTarget::Parent,
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Directory(UseDirectoryDecl {
                    source_name: "hippo_data".into(),
                    source: UseSource::Parent,
                    target_path: CapabilityPath::try_from("/data/hippo").unwrap(),
                    dependency_type: DependencyType::Strong,
                    rights: fio::R_STAR_DIR,
                    subdir: None,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child_with_args(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Directory(fdecl::OfferDirectory {
                source_name: Some("hippo_data".to_string()),
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: Some("coll".to_string()),
                })),
                target_name: Some("hippo_data".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Ok),
    )
    .await;

    test.destroy_dynamic_child(AbsoluteMoniker::root(), "coll", "c").await;
    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;

    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Err(zx::Status::UNAVAILABLE)),
    )
    .await;
}

///    a
///   / \
///  b  [c]
///       \
///        d
/// a: creates [c], with a dynamic offer from b.
/// b: exposes /svc/hippo
/// [c]: instance in collection, offers /svc/hippo to d.
/// d: static child of dynamic instance [c]. uses /svc/hippo.
#[fuchsia::test]
async fn dynamic_offer_to_static_offer() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "fuchsia.component.Realm".into(),
                    source: UseSource::Framework,
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_collection(
                    CollectionDeclBuilder::new_transient_collection("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic)
                        .build(),
                )
                .add_lazy_child("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("hippo_svc").build())
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: ExposeSource::Self_,
                    target_name: "hippo_svc".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: OfferSource::Parent,
                    target_name: "hippo_svc".into(),
                    target: OfferTarget::static_child("d".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_lazy_child("d")
                .build(),
        ),
        (
            "d",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source_name: "hippo_svc".into(),
                    source: UseSource::Parent,
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_lazy_child("d")
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child_with_args(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source_name: Some("hippo_svc".to_string()),
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: None,
                })),
                target_name: Some("hippo_svc".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["coll:c", "d"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

#[fuchsia::test]
async fn destroying_instance_kills_framework_service_task() {
    let components = vec![
        ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Framework,
                    source_name: "fuchsia.component.Realm".into(),
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;

    // Connect to `Realm`, which is a framework service.
    let namespace = test.bind_and_get_namespace(vec!["b"].try_into().unwrap()).await;
    let proxy = capability_util::connect_to_svc_in_namespace::<fcomponent::RealmMarker>(
        &namespace,
        &"/svc/fuchsia.component.Realm".try_into().unwrap(),
    )
    .await;

    // Destroy `b`. This should cause the task hosted for `Realm` to be cancelled.
    let root = test.model.look_up(&AbsoluteMoniker::root()).await.unwrap();
    ActionSet::register(root.clone(), DestroyChildAction::new("b".try_into().unwrap(), 0))
        .await
        .expect("destroy failed");
    let mut event_stream = proxy.take_event_stream();
    assert_matches!(event_stream.next().await, None);
}

#[fuchsia::test]
async fn destroying_instance_blocks_on_routing() {
    // Directories and protocols have a different routing flow so test both.
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::static_child("c".into()),
                    source_name: "foo_svc".into(),
                    target_name: "foo_svc".into(),
                    target: OfferTarget::static_child("b".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source: OfferSource::static_child("c".into()),
                    source_name: "foo_data".into(),
                    target_name: "foo_data".into(),
                    target: OfferTarget::static_child("b".into()),
                    rights: None,
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_lazy_child("b")
                .add_lazy_child("c")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Parent,
                    source_name: "foo_svc".into(),
                    target_path: CapabilityPath::try_from("/svc/echo").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Directory(UseDirectoryDecl {
                    source: UseSource::Parent,
                    source_name: "foo_data".into(),
                    target_path: CapabilityPath::try_from("/data").unwrap(),
                    rights: fio::R_STAR_DIR,
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                .directory(DirectoryDeclBuilder::new("foo_data").build())
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo_svc".into(),
                    target_name: "foo_svc".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo_data".into(),
                    target_name: "foo_data".into(),
                    target: ExposeTarget::Parent,
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
    ];
    // Cause resolution for `c` to block until we explicitly tell it to proceed. This is useful
    // for coordinating the progress of `echo`'s routing task with destruction.
    let builder = RoutingTestBuilder::new("a", components);
    let (resolved_tx, resolved_rx) = oneshot::channel::<()>();
    let (continue_tx, continue_rx) = oneshot::channel::<()>();
    let test = builder.add_blocker("c", resolved_tx, continue_rx).build().await;

    // Connect to `echo` in `b`'s namespace to kick off a protocol routing task.
    let (_, component_name) = test
        .start_and_get_instance(&vec!["b"].try_into().unwrap(), StartReason::Eager, true)
        .await
        .unwrap();
    let component_resolved_url = RoutingTest::resolved_url(&component_name);
    let namespace = test.mock_runner.get_namespace(&component_resolved_url).unwrap();
    let echo_proxy = capability_util::connect_to_svc_in_namespace::<echo::EchoMarker>(
        &namespace,
        &CapabilityPath::try_from("/svc/echo").unwrap(),
    )
    .await;

    // Connect to `data` in `b`'s namespace to kick off a directory routing task.
    let dir_proxy = capability_util::take_dir_from_namespace(&namespace, "/data").await;
    let file_proxy = fuchsia_fs::directory::open_file_no_describe(
        &dir_proxy,
        "hippo",
        fio::OpenFlags::RIGHT_READABLE,
    )
    .unwrap();
    capability_util::add_dir_to_namespace(&namespace, "/data", dir_proxy).await;

    // Destroy `b`.
    let root = test.model.look_up(&AbsoluteMoniker::root()).await.unwrap();
    let mut actions = root.lock_actions().await;
    let destroy_nf =
        actions.register_no_wait(&root, DestroyChildAction::new("b".try_into().unwrap(), 0));
    drop(actions);

    // Give the destroy action some time to complete. Sleeping is not an ideal testing strategy,
    // but it helps add confidence to the test because it makes it more likely the test would
    // fail if the destroy action is not correctly blocking on the routing task.
    fasync::Timer::new(fasync::Time::after(zx::Duration::from_seconds(5))).await;

    // Wait until routing reaches resolution. It should get here because `Destroy` should not
    // cancel the routing task.
    let _ = resolved_rx.await.unwrap();

    // `b` is not yet destroyed.
    let state = root.lock_resolved_state().await.unwrap();
    state.get_child(&ChildMoniker::parse("b").unwrap()).expect("b was destroyed");
    drop(state);

    // Let routing complete. This should allow destruction to complete.
    continue_tx.send(()).unwrap();
    destroy_nf.await.unwrap();

    // Verify the connection to `echo` and `data` was bound by the provider.
    capability_util::call_echo_and_validate_result(echo_proxy, ExpectedResult::Ok).await;
    assert_eq!(fuchsia_fs::file::read_to_string(&file_proxy).await.unwrap(), "hello");
}

///  a
///   \
///    b
///
/// a: declares runner "elf" with service "/svc/runner" from "self".
/// a: registers runner "elf" from self in environment as "hobbit".
/// b: uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_parent_environment() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env").build())
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .add_runner(RunnerRegistration {
                            source_name: "elf".into(),
                            source: RegistrationSource::Self_,
                            target_name: "hobbit".into(),
                        })
                        .build(),
                )
                .runner(RunnerDecl {
                    name: "elf".into(),
                    source_path: Some(CapabilityPath::try_from("/svc/runner").unwrap()),
                })
                .build(),
        ),
        ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "b" exposes a runner service.
        .add_outgoing_path("a", CapabilityPath::try_from("/svc/runner").unwrap(), runner_service)
        .build()
        .await;

    join!(
        // Bind "b". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["b"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///b_resolved".to_string())
            );
        }
    );
}

///  a
///   \
///   [b]
///
/// a: declares runner "elf" with service "/svc/runner" from "self".
/// a: registers runner "elf" from self in environment as "hobbit".
/// b: instance in collection uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_environment_in_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .add_collection(
                    CollectionDeclBuilder::new_transient_collection("coll")
                        .environment("env")
                        .build(),
                )
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .add_runner(RunnerRegistration {
                            source_name: "elf".into(),
                            source: RegistrationSource::Self_,
                            target_name: "hobbit".into(),
                        })
                        .build(),
                )
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Framework,
                    source_name: "fuchsia.component.Realm".into(),
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .runner(RunnerDecl {
                    name: "elf".into(),
                    source_path: Some(CapabilityPath::try_from("/svc/runner").unwrap()),
                })
                .build(),
        ),
        ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path("a", CapabilityPath::try_from("/svc/runner").unwrap(), runner_service)
        .build()
        .await;
    universe
        .create_dynamic_child(
            &AbsoluteMoniker::root(),
            "coll",
            ChildDecl {
                name: "b".to_string(),
                url: "test:///b".to_string(),
                startup: fdecl::StartupMode::Lazy,
                environment: None,
                on_terminate: None,
                config_overrides: None,
            },
        )
        .await;

    join!(
        // Bind "coll:b". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["coll:b"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///b_resolved".to_string())
            );
        }
    );
}

///   a
///    \
///     b
///      \
///       c
///
/// a: declares runner "elf" as service "/svc/runner" from self.
/// a: offers runner "elf" from self to "b" as "dwarf".
/// b: registers runner "dwarf" from realm in environment as "hobbit".
/// c: uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_grandparent_environment() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .add_lazy_child("b")
                .offer(OfferDecl::Runner(OfferRunnerDecl {
                    source: OfferSource::Self_,
                    source_name: "elf".into(),
                    target: OfferTarget::static_child("b".to_string()),
                    target_name: "dwarf".into(),
                }))
                .runner(RunnerDecl {
                    name: "elf".into(),
                    source_path: Some(CapabilityPath::try_from("/svc/runner").unwrap()),
                })
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .add_child(ChildDeclBuilder::new_lazy_child("c").environment("env").build())
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .add_runner(RunnerRegistration {
                            source_name: "dwarf".into(),
                            source: RegistrationSource::Parent,
                            target_name: "hobbit".into(),
                        })
                        .build(),
                )
                .build(),
        ),
        ("c", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path("a", CapabilityPath::try_from("/svc/runner").unwrap(), runner_service)
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["b", "c"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///c_resolved".to_string())
            );
        }
    );
}

///   a
///  / \
/// b   c
///
/// a: registers runner "dwarf" from "b" in environment as "hobbit".
/// b: exposes runner "elf" as service "/svc/runner" from self as "dwarf".
/// c: uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_sibling_environment() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .add_lazy_child("b")
                .add_child(ChildDeclBuilder::new_lazy_child("c").environment("env").build())
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .add_runner(RunnerRegistration {
                            source_name: "dwarf".into(),
                            source: RegistrationSource::Child("b".to_string()),
                            target_name: "hobbit".into(),
                        })
                        .build(),
                )
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Runner(ExposeRunnerDecl {
                    source: ExposeSource::Self_,
                    source_name: "elf".into(),
                    target: ExposeTarget::Parent,
                    target_name: "dwarf".into(),
                }))
                .runner(RunnerDecl {
                    name: "elf".into(),
                    source_path: Some(CapabilityPath::try_from("/svc/runner").unwrap()),
                })
                .build(),
        ),
        ("c", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path("b", CapabilityPath::try_from("/svc/runner").unwrap(), runner_service)
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["c"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///c_resolved".to_string())
            );
        }
    );
}

///   a
///    \
///     b
///      \
///       c
///
/// a: declares runner "elf" as service "/svc/runner" from self.
/// a: registers runner "elf" from realm in environment as "hobbit".
/// b: creates environment extending from realm.
/// c: uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_inherited_environment() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env").build())
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .add_runner(RunnerRegistration {
                            source_name: "elf".into(),
                            source: RegistrationSource::Self_,
                            target_name: "hobbit".into(),
                        })
                        .build(),
                )
                .runner(RunnerDecl {
                    name: "elf".into(),
                    source_path: Some(CapabilityPath::try_from("/svc/runner").unwrap()),
                })
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .add_child(ChildDeclBuilder::new_lazy_child("c").environment("env").build())
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .build(),
                )
                .build(),
        ),
        ("c", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path("a", CapabilityPath::try_from("/svc/runner").unwrap(), runner_service)
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["b", "c"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///c_resolved".to_string())
            );
        }
    );
}

///  a
///   \
///    b
///
/// a: declares runner "runner" with service "/svc/runner" from "self".
/// a: registers runner "runner" from self in environment as "hobbit".
/// b: uses runner "runner". Fails due to a FIDL error, conveyed through a Stop after the
///    bind succeeds.
#[fuchsia::test]
async fn use_runner_from_environment_failed() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env").build())
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .add_runner(RunnerRegistration {
                            source_name: "runner".into(),
                            source: RegistrationSource::Self_,
                            target_name: "runner".into(),
                        })
                        .build(),
                )
                .runner(RunnerDecl {
                    name: "runner".into(),
                    source_path: Some(CapabilityPath::try_from("/svc/runner").unwrap()),
                })
                // For Stopped event
                .use_(UseDecl::EventStream(UseEventStreamDecl {
                    source: UseSource::Parent,
                    source_name: "stopped".into(),
                    scope: None,
                    target_path: CapabilityPath::try_from("/events/stopped").unwrap(),
                    filter: None,
                    availability: Availability::Required,
                }))
                .build(),
        ),
        ("b", ComponentDeclBuilder::new_empty_component().add_program("runner").build()),
    ];

    struct RunnerHost {}
    #[async_trait]
    impl Hook for RunnerHost {
        async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
            if let EventPayload::CapabilityRouted {
                source:
                    CapabilitySource::Component {
                        capability: ComponentCapability::Runner(decl), ..
                    },
                capability_provider,
            } = &event.payload
            {
                let mut capability_provider = capability_provider.lock().await;
                if decl.name.str() == "runner" {
                    *capability_provider = Some(Box::new(RunnerCapabilityProvider {}));
                }
            }
            Ok(())
        }
    }

    struct RunnerCapabilityProvider {}
    #[async_trait]
    impl CapabilityProvider for RunnerCapabilityProvider {
        async fn open(
            self: Box<Self>,
            _task_scope: TaskScope,
            _flags: fio::OpenFlags,
            _relative_path: PathBuf,
            server_end: &mut zx::Channel,
        ) -> Result<(), CapabilityProviderError> {
            channel::take_channel(server_end);
            Ok(())
        }
    }

    // Set a capability provider for the runner that closes the server end.
    // `ComponentRunner.Start` to fail.
    let test = RoutingTestBuilder::new("a", components)
        .set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "stopped".into(),
        })])
        .build()
        .await;

    let runner_host = Arc::new(RunnerHost {});
    test.model
        .root()
        .hooks
        .install(vec![HooksRegistration::new(
            "RunnerHost",
            vec![EventType::CapabilityRouted],
            Arc::downgrade(&runner_host) as Weak<dyn Hook>,
        )])
        .await;
    let namespace_root = test.bind_and_get_namespace(AbsoluteMoniker::root()).await;
    let event_stream =
        capability_util::connect_to_svc_in_namespace::<fcomponent::EventStreamMarker>(
            &namespace_root,
            &"/events/stopped".try_into().unwrap(),
        )
        .await;

    // Even though we expect the runner to fail, bind should succeed. This is because the failure
    // is propagated via the controller channel, separately from the Start action.
    test.start_instance(&vec!["b"].try_into().unwrap()).await.unwrap();

    // Since the controller should have closed, expect a Stopped event.
    let event = event_stream.get_next().await.unwrap().into_iter().next().unwrap();
    assert_matches!(&event,
        fcomponent::Event {
            header: Some(fcomponent::EventHeader {
                moniker: Some(moniker),
                ..
            }),
            payload:
                    Some(fcomponent::EventPayload::Stopped(
                        fcomponent::StoppedPayload {
                            status: Some(status),
                            ..
                        }
                    ))
            ,
            ..
        }
        if *moniker == "./b".to_string()
            && *status == zx::Status::PEER_CLOSED.into_raw() as i32
    );
}

///  a
///   \
///    b
///
/// a: declares runner "elf" with service "/svc/runner" from "self".
/// a: registers runner "elf" from self in environment as "hobbit".
/// b: uses runner "hobbit". Fails because "hobbit" was not in environment.
#[fuchsia::test]
async fn use_runner_from_environment_not_found() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env").build())
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .add_runner(RunnerRegistration {
                            source_name: "elf".into(),
                            source: RegistrationSource::Self_,
                            target_name: "dwarf".into(),
                        })
                        .build(),
                )
                .runner(RunnerDecl {
                    name: "elf".into(),
                    source_path: Some(CapabilityPath::try_from("/svc/runner").unwrap()),
                })
                .build(),
        ),
        ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, _receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path("a", CapabilityPath::try_from("/svc/runner").unwrap(), runner_service)
        .build()
        .await;

    // Bind "b". We expect it to fail because routing failed.
    let err = universe.start_instance(&vec!["b"].try_into().unwrap()).await.unwrap_err();
    let err = match err {
        ModelError::StartActionError {
            err: StartActionError::ResolveRunnerError { err, moniker, .. },
        } if moniker == vec!["b"].try_into().unwrap() => err,
        err => panic!("Unexpected error trying to start b: {}", err),
    };

    assert_matches!(
        *err,
        RouteAndOpenCapabilityError::RoutingError {
            err: RoutingError::UseFromEnvironmentNotFound {
                moniker,
                capability_type,
                capability_name,
            },
            ..
        }
        if moniker == AbsoluteMoniker::try_from(vec!["b"]).unwrap() &&
        capability_type == "runner" &&
        capability_name == "hobbit");
}

// TODO: Write a test for environment that extends from None. Currently, this is not
// straightforward because resolver routing is not implemented yet, which makes it impossible to
// register a new resolver and have it be usable.

///   a
///    \
///    [b]
///      \
///       c
///
/// a: offers service /svc/foo from self
/// [b]: offers service /svc/foo to c
/// [b]: is destroyed
/// c: uses service /svc/foo, which should fail
#[fuchsia::test]
async fn use_with_destroyed_parent() {
    let use_protocol_decl = UseProtocolDecl {
        source: UseSource::Parent,
        source_name: "foo_svc".into(),
        target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
        dependency_type: DependencyType::Strong,
        availability: Availability::Required,
    };
    let use_decl = UseDecl::Protocol(use_protocol_decl.clone());
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Framework,
                    source_name: "fuchsia.component.Realm".into(),
                    target_path: CapabilityPath::try_from("/svc/fuchsia.component.Realm").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".into(),
                    target_name: "foo_svc".into(),
                    target: OfferTarget::Collection("coll".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_transient_collection("coll")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Parent,
                    source_name: "foo_svc".into(),
                    target_name: "foo_svc".into(),
                    target: OfferTarget::static_child("c".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_lazy_child("c")
                .build(),
        ),
        ("c", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
    ];
    let test = RoutingTest::new("a", components).await;
    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;

    // Confirm we can use service from "c".
    test.check_use(
        vec!["coll:b", "c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;

    // Destroy "b", but preserve a reference to "c" so we can route from it below.
    let moniker = vec!["coll:b", "c"].try_into().unwrap();
    let realm_c = test.model.look_up(&moniker).await.expect("failed to look up realm b");
    test.destroy_dynamic_child(AbsoluteMoniker::root(), "coll", "b").await;

    // Now attempt to route the service from "c". Should fail because "b" does not exist so we
    // cannot follow it.
    let err = RouteRequest::UseProtocol(use_protocol_decl)
        .route(&realm_c)
        .await
        .expect_err("routing unexpectedly succeeded");
    assert_matches!(
        err,
        RoutingError::ComponentInstanceError(
            ComponentInstanceError::InstanceNotFound { moniker }
        ) if moniker == vec!["coll:b"].try_into().unwrap()
    );
}

///   a
///  / \
/// b   c
///
/// b: exposes directory /data/foo from self as /data/bar
/// a: offers directory /data/bar from b as /data/baz to c, which was destroyed (but not removed
///    from the tree yet)
/// c: uses /data/baz as /data/hippo
#[fuchsia::test]
async fn use_from_destroyed_but_not_removed() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::static_child("b".to_string()),
                    source_name: "bar_svc".into(),
                    target_name: "baz_svc".into(),
                    target: OfferTarget::static_child("c".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_lazy_child("b")
                .add_lazy_child("c")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .directory(DirectoryDeclBuilder::new("foo_data").build())
                .protocol(ProtocolDeclBuilder::new("foo_svc").build())
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo_svc".into(),
                    target_name: "bar_svc".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Parent,
                    source_name: "baz_svc".into(),
                    target_path: CapabilityPath::try_from("/svc/hippo").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    let component_b = test
        .model
        .look_up(&vec!["b"].try_into().unwrap())
        .await
        .expect("failed to look up realm b");
    // Destroy `b` but keep alive its reference from the parent.
    // TODO: If we had a "pre-destroy" event we could delete the child through normal means and
    // block on the event instead of explicitly registering actions.
    ActionSet::register(component_b.clone(), ShutdownAction::new()).await.expect("shutdown failed");
    ActionSet::register(component_b, DestroyAction::new()).await.expect("destroy failed");
    test.check_use(
        vec!["c"].try_into().unwrap(),
        CheckUse::Protocol {
            path: default_service_capability(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

///   a
///  / \
/// b   c
///
/// a: creates environment "env" and registers resolver "base" from c.
/// b: resolved by resolver "base" through "env".
/// b: exposes resolver "base" from self.
#[fuchsia::test]
async fn use_resolver_from_parent_environment() {
    // Note that we do not define a component "b". This will be resolved by our custom resolver.
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new_empty_component()
                .add_child(ChildDeclBuilder::new().name("b").url("base://b").environment("env"))
                .add_child(ChildDeclBuilder::new_lazy_child("c"))
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .add_resolver(ResolverRegistration {
                            resolver: "base".into(),
                            source: RegistrationSource::Child("c".to_string()),
                            scheme: "base".into(),
                        }),
                )
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Resolver(ExposeResolverDecl {
                    source: ExposeSource::Self_,
                    source_name: "base".into(),
                    target: ExposeTarget::Parent,
                    target_name: "base".into(),
                }))
                .resolver(ResolverDecl {
                    name: "base".into(),
                    source_path: Some(
                        "/svc/fuchsia.component.resolution.Resolver".parse().unwrap(),
                    ),
                })
                .build(),
        ),
    ];

    // Set up the system.
    let (resolver_service, mut receiver) =
        create_service_directory_entry::<fresolution::ResolverMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "c" exposes a resolver service.
        .add_outgoing_path(
            "c",
            CapabilityPath::try_from("/svc/fuchsia.component.resolution.Resolver").unwrap(),
            resolver_service,
        )
        .build()
        .await;

    join!(
        // Bind "b". We expect to see a call to our resolver service for the new component.
        async move {
            universe
                .start_instance(&vec!["b"].try_into().unwrap())
                .await
                .expect("failed to start instance b");
        },
        // Wait for a request, and resolve it.
        async {
            while let Some(request) = receiver.next().await {
                match request {
                    fresolution::ResolverRequest::Resolve { component_url, responder } => {
                        assert_eq!(component_url, "base://b");
                        responder
                            .send(Ok(fresolution::Component {
                                url: Some("test://b".into()),
                                decl: Some(fmem::Data::Bytes(
                                    fidl::persist(&default_component_decl().native_into_fidl())
                                        .unwrap(),
                                )),
                                package: None,
                                // this test only resolves one component_url
                                resolution_context: None,
                                abi_revision: Some(
                                    version_history::LATEST_VERSION.abi_revision.into(),
                                ),
                                ..Default::default()
                            }))
                            .expect("failed to send resolve response");
                    }
                    fresolution::ResolverRequest::ResolveWithContext {
                        component_url,
                        context,
                        responder,
                    } => {
                        warn!(
                            "ResolveWithContext({}, {:?}) request is unexpected in this test",
                            component_url, context
                        );
                        responder
                            .send(Err(fresolution::ResolverError::Internal))
                            .expect("failed to send resolve response");
                    }
                }
            }
        }
    );
}

///   a
///    \
///     b
///      \
///       c
/// a: creates environment "env" and registers resolver "base" from self.
/// b: has environment "env".
/// c: is resolved by resolver from grandarent.
#[fuchsia::test]
async fn use_resolver_from_grandparent_environment() {
    // Note that we do not define a component "c". This will be resolved by our custom resolver.
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .add_child(ChildDeclBuilder::new_lazy_child("b").environment("env"))
                .add_environment(
                    EnvironmentDeclBuilder::new()
                        .name("env")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .add_resolver(ResolverRegistration {
                            resolver: "base".into(),
                            source: RegistrationSource::Self_,
                            scheme: "base".into(),
                        }),
                )
                .resolver(ResolverDecl {
                    name: "base".into(),
                    source_path: Some(
                        "/svc/fuchsia.component.resolution.Resolver".parse().unwrap(),
                    ),
                })
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new_empty_component()
                .add_child(ChildDeclBuilder::new().name("c").url("base://c"))
                .build(),
        ),
    ];

    // Set up the system.
    let (resolver_service, mut receiver) =
        create_service_directory_entry::<fresolution::ResolverMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "c" exposes a resolver service.
        .add_outgoing_path(
            "a",
            CapabilityPath::try_from("/svc/fuchsia.component.resolution.Resolver").unwrap(),
            resolver_service,
        )
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a call to our resolver service for the new component.
        async move {
            universe
                .start_instance(&vec!["b", "c"].try_into().unwrap())
                .await
                .expect("failed to start instance c");
        },
        // Wait for a request, and resolve it.
        async {
            while let Some(request) = receiver.next().await {
                match request {
                    fresolution::ResolverRequest::Resolve { component_url, responder } => {
                        assert_eq!(component_url, "base://c");
                        responder
                            .send(Ok(fresolution::Component {
                                url: Some("test://c".into()),
                                decl: Some(fmem::Data::Bytes(
                                    fidl::persist(&default_component_decl().native_into_fidl())
                                        .unwrap(),
                                )),
                                package: None,
                                // this test only resolves one component_url
                                resolution_context: None,
                                abi_revision: Some(
                                    version_history::LATEST_VERSION.abi_revision.into(),
                                ),
                                ..Default::default()
                            }))
                            .expect("failed to send resolve response");
                    }
                    fresolution::ResolverRequest::ResolveWithContext {
                        component_url,
                        context,
                        responder,
                    } => {
                        warn!(
                            "ResolveWithContext({}, {:?}) request is unexpected in this test",
                            component_url, context
                        );
                        responder
                            .send(Err(fresolution::ResolverError::Internal))
                            .expect("failed to send resolve response");
                    }
                }
            }
        }
    );
}

///   a
///  / \
/// b   c
/// a: creates environment "env" and registers resolver "base" from self.
/// b: has environment "env".
/// c: does NOT have environment "env".
#[fuchsia::test]
async fn resolver_is_not_available() {
    // Note that we do not define a component "b" or "c". This will be resolved by our custom resolver.
    let components = vec![(
        "a",
        ComponentDeclBuilder::new()
            .add_child(ChildDeclBuilder::new().name("b").url("base://b").environment("env"))
            .add_child(ChildDeclBuilder::new().name("c").url("base://c"))
            .add_environment(
                EnvironmentDeclBuilder::new()
                    .name("env")
                    .extends(fdecl::EnvironmentExtends::Realm)
                    .add_resolver(ResolverRegistration {
                        resolver: "base".into(),
                        source: RegistrationSource::Self_,
                        scheme: "base".into(),
                    }),
            )
            .resolver(ResolverDecl {
                name: "base".into(),
                source_path: Some("/svc/fuchsia.component.resolution.Resolver".parse().unwrap()),
            })
            .build(),
    )];

    // Set up the system.
    let (resolver_service, mut receiver) =
        create_service_directory_entry::<fresolution::ResolverMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "c" exposes a resolver service.
        .add_outgoing_path(
            "a",
            CapabilityPath::try_from("/svc/fuchsia.component.resolution.Resolver").unwrap(),
            resolver_service,
        )
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a failure that the scheme is not registered.
        async move {
            match universe.start_instance(&vec!["c"].try_into().unwrap()).await {
                Err(ModelError::ResolveActionError {
                    err:
                        ResolveActionError::ResolverError {
                            url,
                            err: ResolverError::SchemeNotRegistered,
                        },
                }) => {
                    assert_eq!(url, "base://c");
                }
                _ => {
                    panic!("expected ResolverError");
                }
            };
        },
        // Wait for a request, and resolve it.
        async {
            while let Some(request) = receiver.next().await {
                match request {
                    fresolution::ResolverRequest::Resolve { component_url, responder } => {
                        assert_eq!(component_url, "base://b");
                        responder
                            .send(Ok(fresolution::Component {
                                url: Some("test://b".into()),
                                decl: Some(fmem::Data::Bytes(
                                    fidl::persist(&default_component_decl().native_into_fidl())
                                        .unwrap(),
                                )),
                                package: None,
                                // this test only resolves one component_url
                                resolution_context: None,
                                abi_revision: Some(
                                    version_history::LATEST_VERSION.abi_revision.into(),
                                ),
                                ..Default::default()
                            }))
                            .expect("failed to send resolve response");
                    }
                    fresolution::ResolverRequest::ResolveWithContext {
                        component_url,
                        context,
                        responder,
                    } => {
                        warn!(
                            "ResolveWithContext({}, {:?}) request is unexpected in this test",
                            component_url, context
                        );
                        responder
                            .send(Err(fresolution::ResolverError::Internal))
                            .expect("failed to send resolve response");
                    }
                }
            }
        }
    );
}

///   a
///  /
/// b
/// a: creates environment "env" and registers resolver "base" from self.
/// b: has environment "env".
#[fuchsia::test]
async fn resolver_component_decl_is_validated() {
    // Note that we do not define a component "b". This will be resolved by our custom resolver.
    let components = vec![(
        "a",
        ComponentDeclBuilder::new()
            .add_child(ChildDeclBuilder::new().name("b").url("base://b").environment("env"))
            .add_environment(
                EnvironmentDeclBuilder::new()
                    .name("env")
                    .extends(fdecl::EnvironmentExtends::Realm)
                    .add_resolver(ResolverRegistration {
                        resolver: "base".into(),
                        source: RegistrationSource::Self_,
                        scheme: "base".into(),
                    }),
            )
            .resolver(ResolverDecl {
                name: "base".into(),
                source_path: Some("/svc/fuchsia.component.resolution.Resolver".parse().unwrap()),
            })
            .build(),
    )];

    // Set up the system.
    let (resolver_service, mut receiver) =
        create_service_directory_entry::<fresolution::ResolverMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a resolver service.
        .add_outgoing_path(
            "a",
            CapabilityPath::try_from("/svc/fuchsia.component.resolution.Resolver").unwrap(),
            resolver_service,
        )
        .build()
        .await;

    join!(
        // Bind "b". We expect to see a ResolverError.
        async move {
            match universe.start_instance(&vec!["b"].try_into().unwrap()).await {
                Err(ModelError::ResolveActionError {
                    err:
                        ResolveActionError::ResolverError {
                            url,
                            err: ResolverError::ManifestInvalid(_),
                        },
                }) => {
                    assert_eq!(url, "base://b");
                }
                _ => {
                    panic!("expected ResolverError");
                }
            };
        },
        // Wait for a request, and resolve it.
        async {
            while let Some(request) = receiver.next().await {
                match request {
                    fresolution::ResolverRequest::Resolve { component_url, responder } => {
                        assert_eq!(component_url, "base://b");
                        responder
                            .send(Ok(fresolution::Component {
                                url: Some("test://b".into()),
                                decl: Some(fmem::Data::Bytes({
                                    let fidl = fdecl::Component {
                                        exposes: Some(vec![fdecl::Expose::Protocol(
                                            fdecl::ExposeProtocol {
                                                source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
                                                ..Default::default()
                                            },
                                        )]),
                                        ..Default::default()
                                    };
                                    fidl::persist(&fidl).unwrap()
                                })),
                                package: None,
                                // this test only resolves one component_url
                                resolution_context: None,
                                abi_revision: Some(
                                    version_history::LATEST_VERSION.abi_revision.into(),
                                ),
                                ..Default::default()
                            }))
                            .expect("failed to send resolve response");
                    }
                    fresolution::ResolverRequest::ResolveWithContext {
                        component_url,
                        context,
                        responder,
                    } => {
                        warn!(
                            "ResolveWithContext({}, {:?}) request is unexpected in this test",
                            component_url, context
                        );
                        responder
                            .send(Err(fresolution::ResolverError::Internal))
                            .expect("failed to send resolve response");
                    }
                }
            }
        }
    );
}

enum RouteType {
    Offer,
    Expose,
}

async fn verify_service_route(
    test: &RoutingTest,
    use_decl: UseServiceDecl,
    target_moniker: &str,
    agg_moniker: &str,
    child_monikers: &[&str],
    route_type: RouteType,
) {
    let target_moniker: AbsoluteMoniker = target_moniker.try_into().unwrap();
    let agg_moniker: AbsoluteMoniker = agg_moniker.try_into().unwrap();
    let child_monikers: Vec<_> =
        child_monikers.into_iter().map(|m| ChildMoniker::parse(m).unwrap()).collect();

    // Test routing directly.
    let target_component = test.model.look_up(&target_moniker).await.unwrap();
    let agg_component = test.model.look_up(&agg_moniker).await.unwrap();
    let source = RouteRequest::UseService(use_decl).route(&target_component).await.unwrap();
    match source {
        RouteSource {
            source: CapabilitySource::CollectionAggregate { collections, capability, component, .. },
            relative_path: _,
        } => {
            let unique_colls: HashSet<_> =
                child_monikers.iter().map(|c| c.collection().unwrap().to_string()).collect();
            let mut unique_colls: Vec<_> = unique_colls.into_iter().collect();
            unique_colls.sort();
            assert_eq!(collections, unique_colls);
            assert_matches!(capability, AggregateCapability::Service(_));
            assert_eq!(*capability.source_name(), "foo");
            assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &agg_component));
        }
        _ => panic!("wrong capability source"),
    };

    // Populate the collection(s) with dynamic children.
    for child_moniker in &child_monikers {
        let coll = child_moniker.collection().unwrap();
        let name = child_moniker.name();
        test.create_dynamic_child(&agg_moniker, coll, ChildDeclBuilder::new_lazy_child(name)).await;
        test.start_instance_and_wait_start(&agg_moniker.child(child_moniker.clone()))
            .await
            .unwrap();
    }

    // Use the aggregated service from `target_moniker`.
    test.check_use(
        target_moniker,
        CheckUse::Service {
            path: "/svc/foo".try_into().unwrap(),
            instance: ServiceInstance::Aggregated(child_monikers.len()),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
    match route_type {
        RouteType::Offer => {}
        RouteType::Expose => {
            test.check_use_exposed_dir(
                agg_moniker,
                CheckUse::Service {
                    path: "/foo".try_into().unwrap(),
                    instance: ServiceInstance::Aggregated(child_monikers.len()),
                    member: "echo".to_string(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        }
    }
}

///   a
///  / \
/// b   coll
///
/// root: offer service `foo` from `coll` to b
/// b: route `use service`
#[fuchsia::test]
async fn offer_service_from_collection() {
    let use_decl = UseServiceDecl {
        dependency_type: DependencyType::Strong,
        source: UseSource::Parent,
        source_name: "foo".into(),
        target_path: CapabilityPath::try_from("/svc/foo").unwrap(),
        availability: Availability::Required,
    };
    let mut components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_realm()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::Collection("coll".into()),
                    source_name: "foo".into(),
                    source_instance_filter: None,
                    renamed_instances: None,
                    target_name: "foo".into(),
                    target: OfferTarget::static_child("b".into()),
                    availability: Availability::Required,
                }))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll"))
                .add_lazy_child("b")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new().use_(use_decl.clone().into()).build()),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".into(),
                    target_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDeclBuilder::new("foo").build())
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/b",
        "/",
        &["coll:c1", "coll:c2", "coll:c3"],
        RouteType::Offer,
    )
    .await;
}

///   a
///  / \
/// b   (coll1, coll2, coll3)
///
/// root: offer service `foo` from collections to b
/// b: route `use service`
#[fuchsia::test]
async fn offer_service_from_collections() {
    let use_decl = UseServiceDecl {
        dependency_type: DependencyType::Strong,
        source: UseSource::Parent,
        source_name: "foo".into(),
        target_path: CapabilityPath::try_from("/svc/foo").unwrap(),
        availability: Availability::Required,
    };
    let mut offers: Vec<_> = ["coll1", "coll2", "coll3"]
        .into_iter()
        .map(|coll| {
            OfferDecl::Service(OfferServiceDecl {
                source: OfferSource::Collection(coll.into()),
                source_name: "foo".into(),
                source_instance_filter: None,
                renamed_instances: None,
                target_name: "foo".into(),
                target: OfferTarget::static_child("b".into()),
                availability: Availability::Required,
            })
        })
        .collect();
    let mut components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_realm()
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll1"))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll2"))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll3"))
                .add_lazy_child("b")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new().use_(use_decl.clone().into()).build()),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".into(),
                    target_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDeclBuilder::new("foo").build())
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/b",
        "/",
        &["coll1:c1", "coll2:c2", "coll3:c3"],
        RouteType::Offer,
    )
    .await;
}

///     a
///    / \
///   m  (coll1, coll2, coll3)
///  /
/// b
///
/// root: offer service `foo` from coll to b
/// b: route `use service`
#[fuchsia::test]
async fn offer_service_from_collections_multilevel() {
    let use_decl = UseServiceDecl {
        dependency_type: DependencyType::Strong,
        source: UseSource::Parent,
        source_name: "foo".into(),
        target_path: CapabilityPath::try_from("/svc/foo").unwrap(),
        availability: Availability::Required,
    };
    let mut offers: Vec<_> = ["coll1", "coll2", "coll3"]
        .into_iter()
        .map(|coll| {
            OfferDecl::Service(OfferServiceDecl {
                source: OfferSource::Collection(coll.into()),
                source_name: "foo".into(),
                source_instance_filter: None,
                renamed_instances: None,
                target_name: "foo".into(),
                target: OfferTarget::static_child("m".into()),
                availability: Availability::Required,
            })
        })
        .collect();
    let mut components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_realm()
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll1"))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll2"))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll3"))
                .add_lazy_child("m")
                .build(),
        ),
        (
            "m",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::Parent,
                    source_name: "foo".into(),
                    source_instance_filter: None,
                    renamed_instances: None,
                    target_name: "foo".into(),
                    target: OfferTarget::static_child("b".into()),
                    availability: Availability::Required,
                }))
                .add_lazy_child("b")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new().use_(use_decl.clone().into()).build()),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".into(),
                    target_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDeclBuilder::new("foo").build())
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/m/b",
        "/",
        &["coll1:c1", "coll2:c2", "coll3:c3"],
        RouteType::Offer,
    )
    .await;
}

/// a
///  \
///   b
///    \
///    coll
///
/// b: expose service `foo` from `coll` to b
/// a: route `use service`
#[fuchsia::test]
async fn expose_service_from_collection() {
    let use_decl = UseServiceDecl {
        dependency_type: DependencyType::Strong,
        source: UseSource::Child("b".into()),
        source_name: "foo".into(),
        target_path: CapabilityPath::try_from("/svc/foo").unwrap(),
        availability: Availability::Required,
    };
    let mut components = vec![
        (
            "a",
            ComponentDeclBuilder::new().use_(use_decl.clone().into()).add_lazy_child("b").build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_realm()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Collection("coll".into()),
                    source_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    target_name: "foo".into(),
                    availability: Availability::Required,
                }))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll"))
                .build(),
        ),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".into(),
                    target_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDeclBuilder::new("foo").build())
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/",
        "/b",
        &["coll:c1", "coll:c2", "coll:c3"],
        RouteType::Expose,
    )
    .await;
}

/// a
///  \
///   b
///    \
///    (coll1, coll2, coll3)
///
/// b: expose service `foo` from collections to b
/// a: route `use service`
#[fuchsia::test]
async fn expose_service_from_collections() {
    let use_decl = UseServiceDecl {
        dependency_type: DependencyType::Strong,
        source: UseSource::Child("b".into()),
        source_name: "foo".into(),
        target_path: CapabilityPath::try_from("/svc/foo").unwrap(),
        availability: Availability::Required,
    };
    let mut exposes: Vec<_> = ["coll1", "coll2", "coll3"]
        .into_iter()
        .map(|coll| {
            ExposeDecl::Service(ExposeServiceDecl {
                source: ExposeSource::Collection(coll.into()),
                source_name: "foo".into(),
                target: ExposeTarget::Parent,
                target_name: "foo".into(),
                availability: Availability::Required,
            })
        })
        .collect();
    let mut components = vec![
        (
            "a",
            ComponentDeclBuilder::new().use_(use_decl.clone().into()).add_lazy_child("b").build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_realm()
                .expose(exposes.remove(0))
                .expose(exposes.remove(0))
                .expose(exposes.remove(0))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll1"))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll2"))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll3"))
                .build(),
        ),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".into(),
                    target_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDeclBuilder::new("foo").build())
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/",
        "/b",
        &["coll1:c1", "coll2:c2", "coll3:c3"],
        RouteType::Expose,
    )
    .await;
}

/// a
///  \
///   m
///    \
///     b
///      \
///      (coll1, coll2, coll3)
///
/// b: expose service `foo` from collections to b
/// a: route `use service`
#[fuchsia::test]
async fn expose_service_from_collections_multilevel() {
    let use_decl = UseServiceDecl {
        dependency_type: DependencyType::Strong,
        source: UseSource::Child("m".into()),
        source_name: "foo".into(),
        target_path: CapabilityPath::try_from("/svc/foo").unwrap(),
        availability: Availability::Required,
    };
    let mut exposes: Vec<_> = ["coll1", "coll2", "coll3"]
        .into_iter()
        .map(|coll| {
            ExposeDecl::Service(ExposeServiceDecl {
                source: ExposeSource::Collection(coll.into()),
                source_name: "foo".into(),
                target: ExposeTarget::Parent,
                target_name: "foo".into(),
                availability: Availability::Required,
            })
        })
        .collect();
    let mut components = vec![
        (
            "a",
            ComponentDeclBuilder::new().use_(use_decl.clone().into()).add_lazy_child("m").build(),
        ),
        (
            "m",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Child("b".into()),
                    source_name: "foo".into(),
                    target_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .add_lazy_child("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_realm()
                .expose(exposes.remove(0))
                .expose(exposes.remove(0))
                .expose(exposes.remove(0))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll1"))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll2"))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll3"))
                .build(),
        ),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".into(),
                    target_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDeclBuilder::new("foo").build())
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/",
        "/m/b",
        &["coll1:c1", "coll2:c2", "coll3:c3"],
        RouteType::Expose,
    )
    .await;
}

///      root
///      /  \
/// client   (coll1: [service_child_a, non_service_child], coll2: [service_child_b])
///
/// root: offer service `foo` from `(coll1,coll2)` to `client`
/// client: use service
#[fuchsia::test]
async fn list_service_instances_from_collections() {
    let use_decl = UseServiceDecl {
        dependency_type: DependencyType::Strong,
        source: UseSource::Parent,
        source_name: "foo".into(),
        target_path: CapabilityPath::try_from("/foo").unwrap(),
        availability: Availability::Required,
    };
    let mut offers: Vec<_> = ["coll1", "coll2"]
        .into_iter()
        .map(|coll| {
            OfferDecl::Service(OfferServiceDecl {
                source: OfferSource::Collection(coll.into()),
                source_name: "foo".into(),
                source_instance_filter: None,
                renamed_instances: None,
                target_name: "foo".into(),
                target: OfferTarget::static_child("client".into()),
                availability: Availability::Required,
            })
        })
        .collect();
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_realm()
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll1"))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll2"))
                .add_lazy_child("client")
                .build(),
        ),
        ("client", ComponentDeclBuilder::new().use_(use_decl.clone().into()).build()),
        (
            "service_child_a",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    target_name: "foo".into(),
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDecl {
                    name: "foo".into(),
                    source_path: Some("/svc/foo.service".try_into().unwrap()),
                })
                .build(),
        ),
        (
            "service_child_b",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".into(),
                    target: ExposeTarget::Parent,
                    target_name: "foo".into(),
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDecl {
                    name: "foo".into(),
                    source_path: Some("/svc/foo.service".try_into().unwrap()),
                })
                .build(),
        ),
        ("non_service_child", ComponentDeclBuilder::new().build()),
    ];
    let test = RoutingTestBuilder::new("root", components).build().await;

    // Start a few dynamic children in the collections.
    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "coll1",
        ChildDeclBuilder::new_lazy_child("service_child_a"),
    )
    .await;
    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "coll1",
        ChildDeclBuilder::new_lazy_child("non_service_child"),
    )
    .await;
    test.create_dynamic_child(
        &AbsoluteMoniker::root(),
        "coll2",
        ChildDeclBuilder::new_lazy_child("service_child_b"),
    )
    .await;

    let client_component =
        test.model.look_up(&vec!["client"].try_into().unwrap()).await.expect("client instance");
    let source = RouteRequest::UseService(use_decl)
        .route(&client_component)
        .await
        .expect("failed to route service");
    let aggregate_capability_provider = match source {
        RouteSource {
            source: CapabilitySource::CollectionAggregate { aggregate_capability_provider, .. },
            relative_path: _,
        } => aggregate_capability_provider,
        _ => panic!("bad capability source"),
    };

    // Check that only the instances that expose the service are listed.
    let instances: HashSet<ChildMoniker> =
        aggregate_capability_provider.list_instances().await.unwrap().into_iter().collect();
    assert_eq!(instances.len(), 2);
    assert!(instances.contains(&"coll1:service_child_a".try_into().unwrap()));
    assert!(instances.contains(&"coll2:service_child_b".try_into().unwrap()));

    // Try routing to one of the instances.
    let source = aggregate_capability_provider
        .route_instance(&"coll1:service_child_a".try_into().unwrap())
        .await
        .expect("failed to route to child");
    match source {
        CapabilitySource::Component {
            capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
            component,
        } => {
            assert_eq!(name, "foo");
            assert_eq!(
                source_path.expect("source path"),
                "/svc/foo.service".parse::<CapabilityPath>().unwrap()
            );
            assert_eq!(component.abs_moniker, vec!["coll1:service_child_a"].try_into().unwrap());
        }
        _ => panic!("bad child capability source"),
    }
}

///   a
///  / \
/// b   c
///      \
///       coll: [foo, bar, baz]
///
/// a: offer service from c to b
/// b: use service
/// c: expose service from collection `coll`
/// foo, bar: expose service to parent
/// baz: does not expose service
#[fuchsia::test]
async fn use_service_from_sibling_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("c".to_string()),
                    source_name: "my.service.Service".into(),
                    source_instance_filter: None,
                    renamed_instances: None,
                    target: OfferTarget::static_child("b".to_string()),
                    target_name: "my.service.Service".into(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("b"))
                .add_child(ChildDeclBuilder::new_lazy_child("c"))
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Service(UseServiceDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "my.service.Service".into(),
                    target_path: "/svc/my.service.Service".try_into().unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Framework,
                    source_name: "fuchsia.component.Realm".into(),
                    target_path: "/svc/fuchsia.component.Realm".try_into().unwrap(),
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Collection("coll".to_string()),
                    source_name: "my.service.Service".into(),
                    target_name: "my.service.Service".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .add_collection(CollectionDeclBuilder::new_transient_collection("coll"))
                .build(),
        ),
        (
            "foo",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "my.service.Service".into(),
                    target_name: "my.service.Service".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDecl {
                    name: "my.service.Service".into(),
                    source_path: Some("/svc/my.service.Service".try_into().unwrap()),
                })
                .build(),
        ),
        (
            "bar",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "my.service.Service".into(),
                    target_name: "my.service.Service".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDecl {
                    name: "my.service.Service".into(),
                    source_path: Some("/svc/my.service.Service".try_into().unwrap()),
                })
                .build(),
        ),
        ("baz", ComponentDeclBuilder::new().build()),
    ];

    let (directory_entry, mut receiver) = create_service_directory_entry::<echo::EchoMarker>();
    let instance_dir = pseudo_directory! {
        "echo" => directory_entry,
    };
    let test = RoutingTestBuilder::new("a", components)
        .add_outgoing_path(
            "foo",
            "/svc/my.service.Service/default".try_into().unwrap(),
            instance_dir.clone(),
        )
        .add_outgoing_path(
            "bar",
            "/svc/my.service.Service/default".try_into().unwrap(),
            instance_dir,
        )
        .build()
        .await;

    // Populate the collection with dynamic children.
    test.create_dynamic_child(
        &vec!["c"].try_into().unwrap(),
        "coll",
        ChildDeclBuilder::new_lazy_child("foo"),
    )
    .await;
    test.start_instance_and_wait_start(&vec!["c", "coll:foo"].try_into().unwrap())
        .await
        .expect("failed to start `foo`");
    test.create_dynamic_child(
        &vec!["c"].try_into().unwrap(),
        "coll",
        ChildDeclBuilder::new_lazy_child("bar"),
    )
    .await;
    test.start_instance_and_wait_start(&vec!["c", "coll:bar"].try_into().unwrap())
        .await
        .expect("failed to start `bar`");
    test.create_dynamic_child(
        &vec!["c"].try_into().unwrap(),
        "coll",
        ChildDeclBuilder::new_lazy_child("baz"),
    )
    .await;

    join!(
        async move {
            test.check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Service {
                    path: "/svc/my.service.Service".try_into().unwrap(),
                    instance: ServiceInstance::Aggregated(2),
                    member: "echo".to_string(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        },
        async move {
            while let Some(echo::EchoRequest::EchoString { value, responder }) =
                receiver.next().await
            {
                responder.send(value.as_ref().map(|v| v.as_str())).expect("failed to send reply")
            }
        }
    );
}

///   a
/// / | \
/// b c d
///
/// a: offer service from c to b with filter parameters set on offer
/// b: expose service
/// c: use service (with filter)
/// d: use service (with instance renamed)
#[fuchsia::test]
async fn use_filtered_service_from_sibling() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("b".to_string()),
                    source_name: "my.service.Service".into(),
                    source_instance_filter: Some(vec!["variantinstance".to_string()]),
                    renamed_instances: None,
                    target: OfferTarget::static_child("c".to_string()),
                    target_name: "my.service.Service".into(),
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("b".to_string()),
                    source_name: "my.service.Service".into(),
                    source_instance_filter: None, //Some(vec!["variantinstance".to_string()]),
                    renamed_instances: Some(vec![NameMapping {
                        source_name: "default".to_string(),
                        target_name: "renamed_default".to_string(),
                    }]),
                    target: OfferTarget::static_child("d".to_string()),
                    target_name: "my.service.Service".into(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("b"))
                .add_child(ChildDeclBuilder::new_lazy_child("c"))
                .add_child(ChildDeclBuilder::new_lazy_child("d"))
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "my.service.Service".into(),
                    target_name: "my.service.Service".into(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service(ServiceDecl {
                    name: "my.service.Service".into(),
                    source_path: Some("/svc/my.service.Service".try_into().unwrap()),
                })
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Service(UseServiceDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "my.service.Service".into(),
                    target_path: "/svc/my.service.Service".try_into().unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
        (
            "d",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Service(UseServiceDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "my.service.Service".into(),
                    target_path: "/svc/my.service.Service".try_into().unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let (directory_entry, mut receiver) = create_service_directory_entry::<echo::EchoMarker>();
    let instance_dir = pseudo_directory! {
        "echo" => directory_entry,
    };
    let test = RoutingTestBuilder::new("a", components)
        .add_outgoing_path(
            "b",
            "/svc/my.service.Service/default".try_into().unwrap(),
            instance_dir.clone(),
        )
        .add_outgoing_path(
            "b",
            "/svc/my.service.Service/variantinstance".try_into().unwrap(),
            instance_dir,
        )
        .build()
        .await;

    // Check that instance c only has access to the filtered service instance.
    let namespace_c = test.bind_and_get_namespace(vec!["c"].try_into().unwrap()).await;
    let dir_c = capability_util::take_dir_from_namespace(&namespace_c, "/svc").await;
    let service_dir_c = fuchsia_fs::directory::open_directory(
        &dir_c,
        "my.service.Service",
        fuchsia_fs::OpenFlags::empty(),
    )
    .await
    .expect("failed to open service");
    let entries: HashSet<String> = fuchsia_fs::directory::readdir(&service_dir_c)
        .await
        .expect("failed to read entries")
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(entries.contains("variantinstance"));
    capability_util::add_dir_to_namespace(&namespace_c, "/svc", dir_c).await;

    // Check that instance d connects to the renamed instances correctly
    let namespace_d = test.bind_and_get_namespace(vec!["d"].try_into().unwrap()).await;
    let dir_d = capability_util::take_dir_from_namespace(&namespace_d, "/svc").await;
    let service_dir_d = fuchsia_fs::directory::open_directory(
        &dir_d,
        "my.service.Service",
        fuchsia_fs::OpenFlags::empty(),
    )
    .await
    .expect("failed to open service");
    let entries: HashSet<String> = fuchsia_fs::directory::readdir(&service_dir_d)
        .await
        .expect("failed to read entries")
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(entries.len(), 2);
    assert!(entries.contains("renamed_default"));
    assert!(entries.contains("variantinstance"));
    capability_util::add_dir_to_namespace(&namespace_d, "/svc", dir_d).await;

    let _server_handle = fasync::Task::spawn(async move {
        while let Some(echo::EchoRequest::EchoString { value, responder }) = receiver.next().await {
            responder.send(value.as_ref().map(|v| v.as_str())).expect("failed to send reply");
        }
    });
    test.check_use(
        vec!["c"].try_into().unwrap(),
        CheckUse::Service {
            path: "/svc/my.service.Service".try_into().unwrap(),
            instance: ServiceInstance::Named("variantinstance".to_string()),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
    test.check_use(
        vec!["d"].try_into().unwrap(),
        CheckUse::Service {
            path: "/svc/my.service.Service".try_into().unwrap(),
            instance: ServiceInstance::Named("renamed_default".to_string()),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
    test.check_use(
        vec!["d"].try_into().unwrap(),
        CheckUse::Service {
            path: "/svc/my.service.Service".try_into().unwrap(),
            instance: ServiceInstance::Named("variantinstance".to_string()),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
}
