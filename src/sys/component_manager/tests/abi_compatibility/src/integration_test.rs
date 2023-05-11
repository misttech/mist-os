use {
    anyhow::Error,
    assert_matches::assert_matches,
    cm_rust, fidl, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_resolution as fresolution, fidl_fuchsia_mem as fmem,
    fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync,
    fuchsia_component::server as fserver,
    fuchsia_component_test::*,
    futures::{channel::mpsc, FutureExt, SinkExt, StreamExt, TryStreamExt},
    std::{convert::TryInto, fmt},
};

#[derive(Clone, Copy, Debug)]
enum TargetAbi {
    Unsupported,
    Supported,
    Absent,
}

impl fmt::Display for TargetAbi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = match self {
            TargetAbi::Unsupported => "unsupported",
            TargetAbi::Supported => "supported",
            TargetAbi::Absent => "absent",
        };
        f.write_str(id)
    }
}

/// A component resolver serving the `fuchsia.component.resolution.Resolver` protocol.
#[derive(Clone, Copy)]
struct ComponentResolver {
    // Used to set `abi_revision` for components returned by this resolver.
    target_abi: TargetAbi,
}

impl ComponentResolver {
    pub fn new(target_abi: TargetAbi) -> Self {
        Self { target_abi }
    }
    // the scheme is the name of the target abi sent by this resolver
    pub fn scheme(&self) -> String {
        self.target_abi.to_string()
    }
    pub fn name(&self) -> String {
        format!("{}_abi_resolver", self.target_abi)
    }
    pub fn environment(&self) -> String {
        format!("{}_abi_env", self.target_abi)
    }
    pub fn abi_revision(&self) -> Option<u64> {
        match self.target_abi {
            // Assumes the platform does not support a u64::MAX ABI value.
            TargetAbi::Unsupported => Some(u64::MAX),
            // Assumes the latest version is supported.
            TargetAbi::Supported => Some(*version_history::LATEST_VERSION.abi_revision),
            TargetAbi::Absent => None,
        }
    }
    // Return a mock component specific to this resolver. Implemented as a function because
    // fresolution::Component doesn't implement Clone.
    fn mock_component(&self) -> fresolution::Component {
        fresolution::Component {
            url: Some("test".to_string()),
            decl: Some(fmem::Data::Bytes(
                fidl::encoding::persist(&fdecl::Component::default().clone()).unwrap(),
            )),
            abi_revision: self.abi_revision(),
            ..Default::default()
        }
    }
    pub async fn serve(
        self,
        handles: LocalComponentHandles,
        test_channel: mpsc::Sender<fresolution::Component>,
    ) -> Result<(), Error> {
        let mut fs = fserver::ServiceFs::new();
        let mut tasks = vec![];
        fs.dir("svc").add_fidl_service(move |mut stream: fresolution::ResolverRequestStream| {
            let mut test_channel = test_channel.clone();
            tasks.push(fasync::Task::local(async move {
                while let Some(req) = stream.try_next().await.expect("failed to serve resolver") {
                    match req {
                        fresolution::ResolverRequest::Resolve { component_url: _, responder } => {
                            responder.send(&mut Ok(self.mock_component())).unwrap();
                            test_channel
                                .send(self.mock_component())
                                .await
                                .expect("failed to send results");
                        }
                        fresolution::ResolverRequest::ResolveWithContext {
                            component_url: _,
                            context: _,
                            responder,
                        } => {
                            responder.send(&mut Ok(self.mock_component())).unwrap();
                            test_channel
                                .send(self.mock_component())
                                .await
                                .expect("failed to send results");
                        }
                    }
                }
            }));
        });
        fs.serve_connection(handles.outgoing_dir)?;
        fs.collect::<()>().await;
        Ok(())
    }
}

// Add a component resolver to the realm that will return resolved a component containing an
// abi_revision value associated with an Resolver. This function is called for each type of
// resolver that returns a certain ABI revision value.
//
// For example, a ComponentResolver with a TargetAbi::Absent will return an
// fresolution::Component { abi_revision: None, ..} when responding to resolve requests.
//
// A copy of the fresolution::Component is sent over test_channel_tx for the test to verify.
async fn add_component_resolver(
    builder: &mut RealmBuilder,
    component_resolver: ComponentResolver,
    test_channel: mpsc::Sender<fresolution::Component>,
) {
    // Add the resolver component to the realm
    let child = builder
        .add_local_child(
            component_resolver.name(),
            move |h| component_resolver.clone().serve(h, test_channel.clone()).boxed(),
            ChildOptions::new(),
        )
        .await
        .unwrap();
    // Add resolver decl
    let mut child_decl = builder.get_component_decl(&child).await.unwrap();
    child_decl.capabilities.push(cm_rust::CapabilityDecl::Resolver(cm_rust::ResolverDecl {
        name: component_resolver.name().try_into().unwrap(),
        source_path: Some("/svc/fuchsia.component.resolution.Resolver".try_into().unwrap()),
    }));
    child_decl.exposes.push(cm_rust::ExposeDecl::Resolver(cm_rust::ExposeResolverDecl {
        source: cm_rust::ExposeSource::Self_,
        source_name: component_resolver.name().try_into().unwrap(),
        target: cm_rust::ExposeTarget::Parent,
        target_name: component_resolver.name().try_into().unwrap(),
    }));
    builder.replace_component_decl(&child, child_decl).await.unwrap();
    // Add resolver to the test realm
    let mut realm_decl = builder.get_realm_decl().await.unwrap();
    realm_decl.environments.push(cm_rust::EnvironmentDecl {
        name: component_resolver.environment(),
        extends: fdecl::EnvironmentExtends::Realm,
        runners: vec![],
        resolvers: vec![cm_rust::ResolverRegistration {
            resolver: component_resolver.name().try_into().unwrap(),
            source: cm_rust::RegistrationSource::Child(component_resolver.name()),
            // The component resolver is associated with the scheme indicating the abi_revision it returns.
            // (e.g component url "absent:xxx" is resolved by scheme "absent", served by
            // ComponentResolver { TargetAbi::Absent }
            scheme: component_resolver.clone().scheme(),
        }],
        debug_capabilities: vec![],
        stop_timeout_ms: None,
    });
    builder.replace_realm_decl(realm_decl).await.unwrap();
}

// Construct a realm with three component resolvers that return distinct abi revisions for components.
// A test channel is passed to each resolver to send a copy of its FIDL response for testing.
async fn create_realm(
    cm_url: &str,
    test_channel_tx: mpsc::Sender<fresolution::Component>,
) -> (RealmInstance, fasync::Task<()>) {
    let mut builder = RealmBuilder::new().await.unwrap();

    let cr_absent = ComponentResolver::new(TargetAbi::Absent);
    let cr_supported = ComponentResolver::new(TargetAbi::Supported);
    let cr_unsupported = ComponentResolver::new(TargetAbi::Unsupported);

    add_component_resolver(&mut builder, cr_absent.clone(), test_channel_tx.clone()).await;
    add_component_resolver(&mut builder, cr_supported.clone(), test_channel_tx.clone()).await;
    add_component_resolver(&mut builder, cr_unsupported.clone(), test_channel_tx).await;

    builder
        .add_child(
            "absent_abi_component",
            "absent://absent_abi_component",
            ChildOptions::new().environment(cr_absent.environment()),
        )
        .await
        .unwrap();
    builder
        .add_child(
            "unsupported_abi_component",
            "unsupported://unsupported_abi_component",
            ChildOptions::new().environment(cr_unsupported.environment()),
        )
        .await
        .unwrap();
    builder
        .add_child(
            "supported_abi_component",
            "supported://supported_abi_component",
            ChildOptions::new().environment(cr_supported.environment()),
        )
        .await
        .unwrap();

    let (cm_builder, task) = builder.with_nested_component_manager(cm_url).await.unwrap();
    cm_builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.sys2.RealmQuery"))
                .capability(Capability::protocol_by_name("fuchsia.sys2.LifecycleController"))
                .from(Ref::child("component_manager"))
                .to(Ref::parent()),
        )
        .await
        .unwrap();

    let instance = cm_builder.build().await.unwrap();
    instance.start_component_tree().await.unwrap();
    (instance, task)
}

#[fuchsia::test]
async fn resolve_components_against_allow_all_policy() {
    // Uses a config that sets `abi_revision_policy` to `allow_all`
    let cm_url = "#meta/component_manager_allow_all.cm";
    // A channel used to verify component resolver fidl responses.
    let (test_channel_tx, mut test_channel_rx) = mpsc::channel(1);
    let (instance, _task) = create_realm(cm_url, test_channel_tx).await;
    // get a handle to lifecycle controller to start components
    let lifecycle_controller = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fsys::LifecycleControllerMarker>()
        .unwrap();
    // get a handle to realmquery to get component info
    let realm_query = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fsys::RealmQueryMarker>()
        .expect("failed to connect to RealmQuery");

    // Test resolution of a component with an absent abi revision
    {
        let child_moniker = "./absent_abi_component";
        // Attempt to resolve the component. Expect the component resolver to have returned a
        // resolved component with an absent ABI, but resolution passes because of `allow_all`
        // compatibility policy.
        let resolve_res = lifecycle_controller.resolve_instance(child_moniker).await.unwrap();
        assert_eq!(resolve_res, Ok(()));
        // verify the copy of the component resolver result that was sent to component manager
        let resolver_response = test_channel_rx.next().await;
        assert_matches!(resolver_response, Some(fresolution::Component { abi_revision: None, .. }));
        let instance = realm_query.get_instance(child_moniker).await.unwrap().unwrap();
        assert!(instance.resolved_info.is_some());
    }
    // Test resolution of a component with an unsupported abi revision
    {
        let child_moniker = "./unsupported_abi_component";
        let resolve_res = lifecycle_controller.resolve_instance(child_moniker).await.unwrap();
        assert_eq!(resolve_res, Ok(()));
        let resolver_response = test_channel_rx.next().await;
        assert_matches!(
            resolver_response,
            Some(fresolution::Component { abi_revision: Some(u64::MAX), .. })
        );
        let instance = realm_query.get_instance(child_moniker).await.unwrap().unwrap();
        assert!(instance.resolved_info.is_some());
    }
    // Test resolution of a component with a supported abi revision
    {
        let child_moniker = "./supported_abi_component";
        let resolve_res = lifecycle_controller.resolve_instance(child_moniker).await.unwrap();
        assert_eq!(resolve_res, Ok(()));
        let resolver_response =
            test_channel_rx.next().await.expect("resolver failed to return an ABI component");
        assert_eq!(
            resolver_response.abi_revision.unwrap(),
            *version_history::LATEST_VERSION.abi_revision
        );
        let instance = realm_query.get_instance(child_moniker).await.unwrap().unwrap();
        assert!(instance.resolved_info.is_some());
    }
}

#[fuchsia::test]
async fn resolve_components_against_enforce_presence_policy() {
    // Uses a config that sets `abi_revision_policy` to `enforce_presence`
    let cm_url = "#meta/component_manager_enforce_presence.cm";
    // A channel used to verify component resolver fidl responses.
    let (test_channel_tx, mut test_channel_rx) = mpsc::channel(1);
    let (instance, _task) = create_realm(cm_url, test_channel_tx).await;
    // get a handle to lifecycle controller to start components
    let lifecycle_controller = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fsys::LifecycleControllerMarker>()
        .unwrap();
    // get a handle to realmquery to get component info
    let realm_query = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fsys::RealmQueryMarker>()
        .expect("failed to connect to RealmQuery");

    // Test resolution of a component with an absent abi revision
    {
        let child_moniker = "./absent_abi_component";
        // Attempt to resolve the component. Expect the component resolver to have returned a
        // resolved component with an absent ABI, but resolution fails because of
        // `enforce_presence` compatibility policy.
        let resolve_res = lifecycle_controller.resolve_instance(child_moniker).await.unwrap();
        assert_eq!(resolve_res, Err(fsys::ResolveError::Internal));
        // verify the copy of the component resolver result that was sent to component manager
        let resolver_response = test_channel_rx.next().await;
        assert_matches!(resolver_response, Some(fresolution::Component { abi_revision: None, .. }));
        let instance = realm_query.get_instance(child_moniker).await.unwrap().unwrap();
        assert!(instance.resolved_info.is_none());
    }
    // Test resolution of a component with an unsupported abi revision
    {
        let child_moniker = "./unsupported_abi_component";
        let resolve_res = lifecycle_controller.resolve_instance(child_moniker).await.unwrap();
        assert_eq!(resolve_res, Ok(()));
        let resolver_response = test_channel_rx.next().await;
        assert_matches!(
            resolver_response,
            Some(fresolution::Component { abi_revision: Some(u64::MAX), .. })
        );
        let instance = realm_query.get_instance(child_moniker).await.unwrap().unwrap();
        assert!(instance.resolved_info.is_some());
    }
    // Test resolution of a component with a supported abi revision
    {
        let child_moniker = "./supported_abi_component";
        let resolve_res = lifecycle_controller.resolve_instance(child_moniker).await.unwrap();
        assert_eq!(resolve_res, Ok(()));
        let resolver_response =
            test_channel_rx.next().await.expect("resolver failed to return an ABI component");
        assert_eq!(
            resolver_response.abi_revision.unwrap(),
            *version_history::LATEST_VERSION.abi_revision
        );
        let instance = realm_query.get_instance(child_moniker).await.unwrap().unwrap();
        assert!(instance.resolved_info.is_some());
    }
}

#[fuchsia::test]
async fn resolve_components_against_enforce_presence_compatibility_policy() {
    // Uses a config that sets `abi_revision_policy` to `enforce_presence_and_compatibility`
    let cm_url = "#meta/component_manager_enforce_presence_compatibility.cm";
    // A channel used to verify component resolver fidl responses.
    let (test_channel_tx, mut test_channel_rx) = mpsc::channel(1);
    let (instance, _task) = create_realm(cm_url, test_channel_tx).await;
    // get a handle to lifecycle controller to start components
    let lifecycle_controller = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fsys::LifecycleControllerMarker>()
        .unwrap();
    // get a handle to realmquery to get component info
    let realm_query = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fsys::RealmQueryMarker>()
        .expect("failed to connect to RealmQuery");

    // Test resolution of a component with an absent abi revision
    {
        let child_moniker = "./absent_abi_component";
        // Attempt to resolve the component. Expect the component resolver to have returned a
        // resolved component with an absent ABI, but resolution fails because of
        // `enforce_presence_and_compatibility` compatibility policy.
        let resolve_res = lifecycle_controller.resolve_instance(child_moniker).await.unwrap();
        assert_eq!(resolve_res, Err(fsys::ResolveError::Internal));
        // verify the copy of the component resolver result that was sent to component manager
        let resolver_response = test_channel_rx.next().await;
        assert_matches!(resolver_response, Some(fresolution::Component { abi_revision: None, .. }));
        let instance = realm_query.get_instance(child_moniker).await.unwrap().unwrap();
        assert!(instance.resolved_info.is_none());
    }
    // Test resolution of a component with an unsupported abi revision
    {
        let child_moniker = "./unsupported_abi_component";
        let resolve_res = lifecycle_controller.resolve_instance(child_moniker).await.unwrap();
        assert_eq!(resolve_res, Err(fsys::ResolveError::Internal));
        let resolver_response = test_channel_rx.next().await;
        assert_matches!(
            resolver_response,
            Some(fresolution::Component { abi_revision: Some(u64::MAX), .. })
        );
        let instance = realm_query.get_instance(child_moniker).await.unwrap().unwrap();
        assert!(instance.resolved_info.is_none());
    }
    // Test resolution of a component with a supported abi revision
    {
        let child_moniker = "./supported_abi_component";
        let resolve_res = lifecycle_controller.resolve_instance(child_moniker).await.unwrap();
        assert_eq!(resolve_res, Ok(()));
        let resolver_response =
            test_channel_rx.next().await.expect("resolver failed to return an ABI component");
        assert_eq!(
            resolver_response.abi_revision.unwrap(),
            *version_history::LATEST_VERSION.abi_revision
        );
        let instance = realm_query.get_instance(child_moniker).await.unwrap().unwrap();
        assert!(instance.resolved_info.is_some());
    }
}
