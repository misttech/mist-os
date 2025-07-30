// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::request_metadata::{
    InheritRights, IntermediateRights, Metadata, METADATA_KEY_TYPE,
};
use crate::component_instance::{ComponentInstanceInterface, WeakExtendedInstanceInterface};
use crate::error::{ErrorReporter, RouteRequestErrorInfo, RoutingError};
use crate::rights::{Rights, RightsWalker};
use crate::subdir::SubDir;
use crate::walk_state::WalkStateUnit;
use async_trait::async_trait;
use cm_rust::CapabilityTypeName;
use cm_types::{Availability, Name};
use fidl_fuchsia_component_sandbox as fsandbox;
use moniker::ExtendedMoniker;
use router_error::RouterError;
use sandbox::{Capability, CapabilityBound, Data, Dict, Request, Routable, Router, RouterResponse};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use strum::IntoEnumIterator;

struct PorcelainRouter<T: CapabilityBound, R, C: ComponentInstanceInterface, const D: bool> {
    router: Router<T>,
    porcelain_type: CapabilityTypeName,
    availability: Availability,
    rights: Option<Rights>,
    subdir: Option<SubDir>,
    inherit_rights: Option<bool>,
    target: WeakExtendedInstanceInterface<C>,
    route_request: RouteRequestErrorInfo,
    error_reporter: R,
}

#[async_trait]
impl<
        T: CapabilityBound,
        R: ErrorReporter,
        C: ComponentInstanceInterface + 'static,
        const D: bool,
    > Routable<T> for PorcelainRouter<T, R, C, D>
{
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        match self.do_route(request, debug, D).await {
            Ok(res) => Ok(res),
            Err(err) => {
                self.error_reporter
                    .report(&self.route_request, &err, self.target.clone().into())
                    .await;
                Err(err)
            }
        }
    }
}

impl<
        T: CapabilityBound,
        R: ErrorReporter,
        C: ComponentInstanceInterface + 'static,
        const D: bool,
    > PorcelainRouter<T, R, C, D>
{
    #[inline]
    async fn do_route(
        &self,
        request: Option<Request>,
        debug: bool,
        supply_default: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        let PorcelainRouter {
            router,
            porcelain_type,
            availability,
            rights,
            subdir,
            inherit_rights,
            target,
            route_request: _,
            error_reporter: _,
        } = self;
        let request = if let Some(request) = request {
            request
        } else {
            if !supply_default {
                Err(RouterError::InvalidArgs)?;
            }
            let metadata = Dict::new();
            metadata
                .insert(
                    Name::new(METADATA_KEY_TYPE).unwrap(),
                    Capability::Data(Data::String(porcelain_type.to_string().into())),
                )
                .expect("failed to build default metadata?");
            metadata.set_metadata(*availability);
            if let Some(rights) = rights {
                metadata.set_metadata(*rights);
            }
            if let Some(inherit_rights) = inherit_rights {
                metadata.set_metadata(InheritRights(*inherit_rights));
            }
            Request { target: target.clone().into(), metadata }
        };

        let moniker: ExtendedMoniker = match &self.target {
            WeakExtendedInstanceInterface::Component(t) => t.moniker.clone().into(),
            WeakExtendedInstanceInterface::AboveRoot(_) => ExtendedMoniker::ComponentManager,
        };
        check_porcelain_type(&moniker, &request, *porcelain_type)?;
        let updated_availability = check_availability(&moniker, &request, *availability)?;

        check_and_compute_rights(&moniker, &request, &rights)?;
        if let Some(new_subdir) = check_and_compute_subdir(&moniker, &request, &subdir)? {
            request.metadata.set_metadata(new_subdir);
        }

        // Everything checks out, forward the request.
        request.metadata.set_metadata(updated_availability);
        router.route(Some(request), debug).await
    }
}

fn check_porcelain_type(
    moniker: &ExtendedMoniker,
    request: &Request,
    expected_type: CapabilityTypeName,
) -> Result<(), RouterError> {
    let Capability::Data(Data::String(capability_type)) = request
        .metadata
        .get(&cm_types::Name::new(METADATA_KEY_TYPE).unwrap())
        .map_err(|()| RoutingError::BedrockNotCloneable { moniker: moniker.clone() })?
        .ok_or_else(|| RoutingError::BedrockMissingCapabilityType {
            moniker: moniker.clone(),
            type_name: expected_type.to_string(),
        })?
    else {
        return Err(RoutingError::BedrockNotPresentInDictionary {
            moniker: moniker.clone(),
            name: String::from("type"),
        }
        .into());
    };
    if *capability_type != expected_type.to_string() {
        Err(RoutingError::BedrockWrongCapabilityType {
            moniker: moniker.clone(),
            actual: capability_type.into(),
            expected: expected_type.to_string(),
        })?;
    }
    Ok(())
}

fn check_availability(
    moniker: &ExtendedMoniker,
    request: &Request,
    availability: Availability,
) -> Result<Availability, RouterError> {
    // The availability of the request must be compatible with the
    // availability of this step of the route.
    let request_availability = request
        .metadata
        .get_metadata()
        .ok_or(fsandbox::RouterError::InvalidArgs)
        .inspect_err(|e| {
            log::error!("request {:?} did not have availability metadata: {e:?}", request.target)
        })?;
    crate::availability::advance(&moniker, request_availability, availability)
        .map_err(|e| RoutingError::from(e).into())
}

fn check_and_compute_rights(
    moniker: &ExtendedMoniker,
    request: &Request,
    rights: &Option<Rights>,
) -> Result<(), RouterError> {
    let Some(rights) = rights else {
        return Ok(());
    };
    let InheritRights(inherit) = request.metadata.get_metadata().ok_or(RouterError::InvalidArgs)?;
    let request_rights: Rights = match request.metadata.get_metadata() {
        Some(request_rights) => request_rights,
        None => {
            if inherit {
                request.metadata.set_metadata(*rights);
                *rights
            } else {
                Err(RouterError::InvalidArgs)?
            }
        }
    };
    let intermediate_rights: Option<IntermediateRights> = request.metadata.get_metadata();
    let request_rights = RightsWalker::new(request_rights, moniker.clone());
    let router_rights = RightsWalker::new(*rights, moniker.clone());
    // The rights of the previous step (if any) of the route must be
    // compatible with this step of the route.
    if let Some(IntermediateRights(intermediate_rights)) = intermediate_rights {
        let intermediate_rights_walker = RightsWalker::new(intermediate_rights, moniker.clone());
        intermediate_rights_walker
            .validate_next(&router_rights)
            .map_err(|e| router_error::RouterError::from(RoutingError::from(e)))?;
    };
    request.metadata.set_metadata(IntermediateRights(*rights));
    // The rights of the request must be compatible with the
    // rights of this step of the route.
    request_rights.validate_next(&router_rights).map_err(RoutingError::from)?;
    Ok(())
}

fn check_and_compute_subdir(
    moniker: &ExtendedMoniker,
    request: &Request,
    subdir: &Option<SubDir>,
) -> Result<Option<SubDir>, RouterError> {
    let Some(mut subdir_from_decl) = subdir.clone() else {
        return Ok(None);
    };

    let request_subdir: Option<SubDir> = request.metadata.get_metadata();

    if let Some(request_subdir) = request_subdir {
        let success = subdir_from_decl.as_mut().extend(request_subdir.clone().into());
        if !success {
            return Err(RoutingError::PathTooLong {
                moniker: moniker.clone(),
                path: subdir_from_decl.to_string(),
                keyword: request_subdir.to_string(),
            }
            .into());
        }
    }
    Ok(Some(subdir_from_decl))
}

pub type DefaultMetadataFn = Arc<dyn Fn(Availability) -> Dict + Send + Sync + 'static>;

/// Builds a router that ensures the capability request has an availability
/// strength that is at least the provided `availability`. A default `Request`
/// is populated with `metadata_fn` if the client passes a `None` `Request`.
pub struct PorcelainBuilder<
    T: CapabilityBound,
    R: ErrorReporter,
    C: ComponentInstanceInterface + 'static,
    const D: bool,
> {
    router: Router<T>,
    porcelain_type: CapabilityTypeName,
    availability: Option<Availability>,
    rights: Option<Rights>,
    subdir: Option<SubDir>,
    inherit_rights: Option<bool>,
    target: Option<WeakExtendedInstanceInterface<C>>,
    error_info: Option<RouteRequestErrorInfo>,
    error_reporter: Option<R>,
}

impl<
        T: CapabilityBound,
        R: ErrorReporter,
        C: ComponentInstanceInterface + 'static,
        const D: bool,
    > PorcelainBuilder<T, R, C, D>
{
    fn new(router: Router<T>, porcelain_type: CapabilityTypeName) -> Self {
        Self {
            router,
            porcelain_type,
            availability: None,
            rights: None,
            subdir: None,
            inherit_rights: None,
            target: None,
            error_info: None,
            error_reporter: None,
        }
    }

    /// The [Availability] attribute for this route.
    /// REQUIRED.
    pub fn availability(mut self, a: Availability) -> Self {
        self.availability = Some(a);
        self
    }

    pub fn rights(mut self, rights: Option<Rights>) -> Self {
        self.rights = rights;
        self
    }

    pub fn subdir(mut self, subdir: SubDir) -> Self {
        self.subdir = Some(subdir);
        self
    }

    pub fn inherit_rights(mut self, inherit_rights: bool) -> Self {
        self.inherit_rights = Some(inherit_rights);
        self
    }

    /// The identity of the component on behalf of whom this routing request is performed, if the
    /// caller passes a `None` request.
    /// Either this or `target_above_root` is REQUIRED.
    pub fn target(mut self, t: &Arc<C>) -> Self {
        self.target = Some(WeakExtendedInstanceInterface::Component(t.as_weak()));
        self
    }

    /// The identity of the "above root" instance that is component manager itself.
    /// Either this or `target` is REQUIRED.
    pub fn target_above_root(mut self, t: &Arc<C::TopInstance>) -> Self {
        self.target = Some(WeakExtendedInstanceInterface::AboveRoot(Arc::downgrade(t)));
        self
    }

    /// Object used to generate diagnostic information about the route that is logged if the route
    /// fails. This is usually a [cm_rust] type that is castable to [RouteRequestErrorInfo]
    /// REQUIRED.
    pub fn error_info<S>(mut self, r: S) -> Self
    where
        RouteRequestErrorInfo: From<S>,
    {
        self.error_info = Some(RouteRequestErrorInfo::from(r));
        self
    }

    /// The [ErrorReporter] used to log errors if routing fails.
    /// REQUIRED.
    pub fn error_reporter(mut self, r: R) -> Self {
        self.error_reporter = Some(r);
        self
    }

    /// Build the [PorcelainRouter] with attributes configured by this builder.
    pub fn build(self) -> Router<T> {
        Router::new(PorcelainRouter::<T, R, C, D> {
            router: self.router,
            porcelain_type: self.porcelain_type,
            availability: self.availability.expect("must set availability"),
            rights: self.rights,
            subdir: self.subdir,
            inherit_rights: self.inherit_rights,
            target: self.target.expect("must set target"),
            route_request: self.error_info.expect("must set route_request"),
            error_reporter: self.error_reporter.expect("must set error_reporter"),
        })
    }
}

impl<
        R: ErrorReporter,
        T: CapabilityBound,
        C: ComponentInstanceInterface + 'static,
        const D: bool,
    > From<PorcelainBuilder<T, R, C, D>> for Capability
where
    Router<T>: Into<Capability>,
{
    fn from(b: PorcelainBuilder<T, R, C, D>) -> Self {
        b.build().into()
    }
}

/// See [WithPorcelain::with_porcelain] for documentation.
pub trait WithPorcelain<
    T: CapabilityBound,
    R: ErrorReporter,
    C: ComponentInstanceInterface + 'static,
>
{
    /// Returns a [PorcelainBuilder] you use to construct a new router with porcelain properties
    /// that augments the `self`. See [PorcelainBuilder] for documentation of the supported
    /// properties.
    ///
    /// If a `None` request is passed into the built router, the router will supply a default
    /// request based on the values passed to the builder.
    fn with_porcelain_with_default(
        self,
        type_: CapabilityTypeName,
    ) -> PorcelainBuilder<T, R, C, true>;

    /// Returns a [PorcelainBuilder] you use to construct a new router with porcelain properties
    /// that augments the `self`. See [PorcelainBuilder] for documentation of the supported
    /// properties.
    ///
    /// If a `None` request is passed into the built router, the router will throw an `InvalidArgs`
    /// error.
    fn with_porcelain_no_default(
        self,
        type_: CapabilityTypeName,
    ) -> PorcelainBuilder<T, R, C, false>;
}

impl<T: CapabilityBound, R: ErrorReporter, C: ComponentInstanceInterface + 'static>
    WithPorcelain<T, R, C> for Router<T>
{
    fn with_porcelain_with_default(
        self,
        type_: CapabilityTypeName,
    ) -> PorcelainBuilder<T, R, C, true> {
        PorcelainBuilder::<T, R, C, true>::new(self, type_)
    }

    fn with_porcelain_no_default(
        self,
        type_: CapabilityTypeName,
    ) -> PorcelainBuilder<T, R, C, false> {
        PorcelainBuilder::<T, R, C, false>::new(self, type_)
    }
}

pub fn metadata_for_porcelain_type(
    typename: CapabilityTypeName,
) -> Arc<dyn Fn(Availability) -> Dict + Send + Sync + 'static> {
    type MetadataMap =
        HashMap<CapabilityTypeName, Arc<dyn Fn(Availability) -> Dict + Send + Sync + 'static>>;
    static CLOSURES: LazyLock<MetadataMap> = LazyLock::new(|| {
        fn entry_for_typename(
            typename: CapabilityTypeName,
        ) -> (CapabilityTypeName, Arc<dyn Fn(Availability) -> Dict + Send + Sync + 'static>)
        {
            let v = Arc::new(move |availability: Availability| {
                let metadata = Dict::new();
                metadata
                    .insert(
                        Name::new(METADATA_KEY_TYPE).unwrap(),
                        Capability::Data(Data::String(typename.to_string().into())),
                    )
                    .expect("failed to build default metadata?");
                metadata.set_metadata(availability);
                metadata
            });
            (typename, v)
        }
        CapabilityTypeName::iter().map(entry_for_typename).collect()
    });
    CLOSURES.get(&typename).unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::sandbox_construction::ComponentSandbox;
    use crate::capability_source::{BuiltinCapabilities, NamespaceCapabilities};
    use crate::component_instance::{ExtendedInstanceInterface, TopInstanceInterface};
    use crate::error::ComponentInstanceError;
    use crate::policy::GlobalPolicyChecker;
    use crate::{environment, ResolvedInstanceInterface};
    use assert_matches::assert_matches;
    use cm_rust_testing::UseBuilder;
    use cm_types::Url;
    use moniker::{BorrowedChildName, Moniker};
    use router_error::{DowncastErrorForTest, RouterError};
    use sandbox::{Data, Dict};
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct FakeComponent {
        moniker: Moniker,
    }

    #[derive(Debug)]
    struct FakeTopInstance {
        ns: NamespaceCapabilities,
        builtin: BuiltinCapabilities,
    }

    impl TopInstanceInterface for FakeTopInstance {
        fn namespace_capabilities(&self) -> &NamespaceCapabilities {
            &self.ns
        }
        fn builtin_capabilities(&self) -> &BuiltinCapabilities {
            &self.builtin
        }
    }

    #[async_trait]
    impl ComponentInstanceInterface for FakeComponent {
        type TopInstance = FakeTopInstance;

        fn child_moniker(&self) -> Option<&BorrowedChildName> {
            panic!()
        }

        fn moniker(&self) -> &Moniker {
            &self.moniker
        }

        fn url(&self) -> &Url {
            panic!()
        }

        fn environment(&self) -> &environment::Environment<Self> {
            panic!()
        }

        fn config_parent_overrides(&self) -> Option<&Vec<cm_rust::ConfigOverride>> {
            panic!()
        }

        fn policy_checker(&self) -> &GlobalPolicyChecker {
            panic!()
        }

        fn component_id_index(&self) -> &component_id_index::Index {
            panic!()
        }

        fn try_get_parent(
            &self,
        ) -> Result<ExtendedInstanceInterface<Self>, ComponentInstanceError> {
            panic!()
        }

        async fn lock_resolved_state<'a>(
            self: &'a Arc<Self>,
        ) -> Result<Box<dyn ResolvedInstanceInterface<Component = Self> + 'a>, ComponentInstanceError>
        {
            panic!()
        }

        async fn component_sandbox(
            self: &Arc<Self>,
        ) -> Result<ComponentSandbox, ComponentInstanceError> {
            panic!()
        }
    }

    #[derive(Clone)]
    struct TestErrorReporter {
        reported: Arc<Mutex<bool>>,
    }

    impl TestErrorReporter {
        fn new() -> Self {
            Self { reported: Arc::new(Mutex::new(false)) }
        }
    }

    #[async_trait]
    impl ErrorReporter for TestErrorReporter {
        async fn report(
            &self,
            _request: &RouteRequestErrorInfo,
            _err: &RouterError,
            _route_target: sandbox::WeakInstanceToken,
        ) {
            let mut reported = self.reported.lock().unwrap();
            if *reported {
                panic!("report() was called twice");
            }
            *reported = true;
        }
    }

    fn fake_component() -> Arc<FakeComponent> {
        Arc::new(FakeComponent { moniker: Moniker::root() })
    }

    fn error_info() -> cm_rust::UseDecl {
        UseBuilder::protocol().name("name").build()
    }

    #[fuchsia::test]
    async fn success() {
        let source = Data::String("hello".into());
        let base = Router::<Data>::new_ok(source);
        let component = fake_component();
        let proxy = base
            .with_porcelain_with_default(CapabilityTypeName::Protocol)
            .availability(Availability::Optional)
            .target(&component)
            .error_info(&error_info())
            .error_reporter(TestErrorReporter::new())
            .build();
        let metadata = Dict::new();
        metadata
            .insert(
                Name::new(METADATA_KEY_TYPE).unwrap(),
                Capability::Data(Data::String(CapabilityTypeName::Protocol.to_string().into())),
            )
            .unwrap();
        metadata.set_metadata(Availability::Optional);

        let capability = proxy
            .route(Some(Request { target: component.as_weak().into(), metadata }), false)
            .await
            .unwrap();
        let capability = match capability {
            RouterResponse::<Data>::Capability(d) => d,
            _ => panic!(),
        };
        assert_eq!(capability, Data::String("hello".into()));
    }

    #[fuchsia::test]
    async fn type_missing() {
        let reporter = TestErrorReporter::new();
        let reported = reporter.reported.clone();
        let source = Data::String("hello".into());
        let base = Router::<Data>::new_ok(source);
        let component = fake_component();
        let proxy = base
            .with_porcelain_with_default(CapabilityTypeName::Protocol)
            .availability(Availability::Optional)
            .target(&component)
            .error_info(&error_info())
            .error_reporter(reporter)
            .build();
        let metadata = Dict::new();
        metadata.set_metadata(Availability::Optional);

        let error = proxy
            .route(Some(Request { target: component.as_weak().into(), metadata }), false)
            .await
            .unwrap_err();
        assert_matches!(
            error,
            RouterError::NotFound(err)
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::BedrockMissingCapabilityType {
                    moniker,
                    type_name,
                } if moniker == &Moniker::root().into() && type_name == "protocol"
            )
        );
        assert!(*reported.lock().unwrap());
    }

    #[fuchsia::test]
    async fn type_mismatch() {
        let reporter = TestErrorReporter::new();
        let reported = reporter.reported.clone();
        let source = Data::String("hello".into());
        let base = Router::<Data>::new_ok(source);
        let component = fake_component();
        let proxy = base
            .with_porcelain_with_default(CapabilityTypeName::Protocol)
            .availability(Availability::Optional)
            .target(&component)
            .error_info(&error_info())
            .error_reporter(reporter)
            .build();
        let metadata = Dict::new();
        metadata
            .insert(
                Name::new(METADATA_KEY_TYPE).unwrap(),
                Capability::Data(Data::String(CapabilityTypeName::Service.to_string().into())),
            )
            .unwrap();
        metadata.set_metadata(Availability::Optional);

        let error = proxy
            .route(Some(Request { target: component.as_weak().into(), metadata }), false)
            .await
            .unwrap_err();
        assert_matches!(
            error,
            RouterError::NotFound(err)
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::BedrockWrongCapabilityType {
                    moniker,
                    expected,
                    actual
                } if moniker == &Moniker::root().into()
                    && expected == "protocol" && actual == "service"
            )
        );
        assert!(*reported.lock().unwrap());
    }

    #[fuchsia::test]
    async fn availability_mismatch() {
        let reporter = TestErrorReporter::new();
        let reported = reporter.reported.clone();
        let source = Data::String("hello".into());
        let base = Router::<Data>::new_ok(source);
        let component = fake_component();
        let proxy = base
            .with_porcelain_with_default(CapabilityTypeName::Protocol)
            .availability(Availability::Optional)
            .target(&component)
            .error_info(&error_info())
            .error_reporter(reporter)
            .build();
        let metadata = Dict::new();
        metadata
            .insert(
                Name::new(METADATA_KEY_TYPE).unwrap(),
                Capability::Data(Data::String(CapabilityTypeName::Protocol.to_string().into())),
            )
            .unwrap();
        metadata.set_metadata(Availability::Required);

        let error = proxy
            .route(Some(Request { target: component.as_weak().into(), metadata }), false)
            .await
            .unwrap_err();
        assert_matches!(
            error,
            RouterError::NotFound(err)
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::AvailabilityRoutingError(
                        crate::error::AvailabilityRoutingError::TargetHasStrongerAvailability {
                        moniker
                    }
                ) if moniker == &Moniker::root().into()
            )
        );
        assert!(*reported.lock().unwrap());
    }
}
