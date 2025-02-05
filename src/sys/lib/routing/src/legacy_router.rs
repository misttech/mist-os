// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Each routing method's name begins with `route_*`, and is an async function that returns
//! [Result<CapabilitySource, RoutingError>], i.e. finds the capability source by walking the
//! route declarations and resolving components if necessary. Routing always walks in the direction
//! from the consuming side to the providing side.
//!
//! The most commonly used implementation is [route_from_use], which starts from a `Use`
//! declaration, walks along the chain of `Offer`s, and then the chain of `Expose`s. Similarly,
//! [route_from_registration] starts from a registration of a capability into an environment, then
//! walks the `Offer`s and `Expose`s.
//!
//! You can also start walking from halfway in this chain, e.g. [route_from_offer],
//! [route_from_expose].

use crate::capability_source::{
    AggregateCapability, AggregateMember, AnonymizedAggregateSource, BuiltinSource,
    CapabilitySource, CapabilityToCapabilitySource, ComponentCapability, ComponentSource,
    FilteredAggregateProviderSource, FilteredProviderSource, FrameworkSource, InternalCapability,
    NamespaceSource, VoidSource,
};
use crate::component_instance::{
    ComponentInstanceInterface, ExtendedInstanceInterface, ResolvedInstanceInterface,
    TopInstanceInterface,
};
use crate::error::RoutingError;
use crate::mapper::DebugRouteMapper;
use crate::RegistrationDecl;
use cm_rust::{
    Availability, CapabilityDecl, CapabilityTypeName, ChildRef, ExposeDecl, ExposeDeclCommon,
    ExposeSource, ExposeTarget, FidlIntoNative, NativeIntoFidl, OfferDecl, OfferDeclCommon,
    OfferServiceDecl, OfferSource, OfferTarget, RegistrationDeclCommon, RegistrationSource,
    SourceName, SourcePath, UseDecl, UseDeclCommon, UseSource,
};
use cm_rust_derive::FidlDecl;
use cm_types::{LongName, Name};
use fidl_fuchsia_component_internal as finternal;
use moniker::{ChildName, ExtendedMoniker, Moniker};
use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::Arc;
use std::{fmt, slice};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Routes a capability from its `Use` declaration to its source by following `Offer` and `Expose`
/// declarations.
///
/// `sources` defines what are the valid sources of the capability. See [`Sources`].
/// `visitor` is invoked for each `Offer` and `Expose` declaration in the routing path, as well as
/// the final `Capability` declaration if `sources` permits.
pub async fn route_from_use<C, V>(
    use_decl: UseDecl,
    use_target: Arc<C>,
    sources: Sources,
    visitor: &mut V,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<CapabilitySource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
{
    let cap_type = CapabilityTypeName::from(&use_decl);
    if !use_decl.source_path().dirname.is_dot() {
        return Err(RoutingError::DictionariesNotSupported {
            cap_type,
            moniker: use_target.moniker().clone(),
        });
    }
    match Use::route(use_decl, use_target.clone(), &sources, visitor, mapper).await? {
        UseResult::Source(source) => return Ok(source),
        UseResult::OfferFromParent(offer, component) => {
            route_from_offer(offer, component, sources, visitor, mapper).await
        }
        UseResult::ExposeFromChild(use_decl, child_component) => {
            let child_exposes = child_component.lock_resolved_state().await?.exposes();
            let child_exposes =
                find_matching_exposes(cap_type, use_decl.source_name(), &child_exposes)
                    .ok_or_else(|| {
                        let child_moniker =
                            child_component.child_moniker().expect("ChildName should exist");
                        <UseDecl as ErrorNotFoundInChild>::error_not_found_in_child(
                            use_target.moniker().clone(),
                            child_moniker.clone(),
                            use_decl.source_name().clone(),
                        )
                    })?;
            route_from_expose(child_exposes, child_component, sources, visitor, mapper).await
        }
    }
}

/// Routes a capability from its environment `Registration` declaration to its source by following
/// `Offer` and `Expose` declarations.
///
/// `sources` defines what are the valid sources of the capability. See [`Sources`].
/// `visitor` is invoked for each `Offer` and `Expose` declaration in the routing path, as well as
/// the final `Capability` declaration if `sources` permits.
pub async fn route_from_registration<R, C, V>(
    registration_decl: R,
    registration_target: Arc<C>,
    sources: Sources,
    visitor: &mut V,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<CapabilitySource, RoutingError>
where
    R: RegistrationDeclCommon
        + ErrorNotFoundFromParent
        + ErrorNotFoundInChild
        + Into<RegistrationDecl>
        + Clone
        + 'static,
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
{
    match Registration::route(registration_decl, registration_target, &sources, visitor, mapper)
        .await?
    {
        RegistrationResult::Source(source) => return Ok(source),
        RegistrationResult::FromParent(offer, component) => {
            route_from_offer(offer, component, sources, visitor, mapper).await
        }
        RegistrationResult::FromChild(expose, component) => {
            route_from_expose(expose, component, sources, visitor, mapper).await
        }
    }
}

/// Routes a capability from its `Offer` declaration to its source by following `Offer` and `Expose`
/// declarations.
///
/// `sources` defines what are the valid sources of the capability. See [`Sources`].
/// `visitor` is invoked for each `Offer` and `Expose` declaration in the routing path, as well as
/// the final `Capability` declaration if `sources` permits.
pub async fn route_from_offer<C, V>(
    offer: RouteBundle<OfferDecl>,
    offer_target: Arc<C>,
    sources: Sources,
    visitor: &mut V,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<CapabilitySource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
{
    let cap_type = CapabilityTypeName::from(offer.iter().next().unwrap());
    for offer in offer.iter() {
        if !offer.source_path().dirname.is_dot() {
            return Err(RoutingError::DictionariesNotSupported {
                cap_type,
                moniker: offer_target.moniker().clone(),
            });
        }
    }
    match Offer::route(offer, offer_target.clone(), &sources, visitor, mapper).await? {
        OfferResult::Source(source) => return Ok(source),
        OfferResult::OfferFromChild(offer, component) => {
            let offer_decl: OfferDecl = offer.clone().into();

            let (exposes, component) = change_directions::<C>(offer, component).await?;

            let capability_source =
                route_from_expose(exposes, component, sources, visitor, mapper).await?;
            if let OfferDecl::Service(offer_service_decl) = offer_decl {
                if offer_service_decl.source_instance_filter.is_some()
                    || offer_service_decl.renamed_instances.is_some()
                {
                    // TODO(https://fxbug.dev/42179343) support collection sources as well.
                    if let CapabilitySource::Component(ComponentSource { capability, moniker }) =
                        capability_source
                    {
                        let source_name = offer_service_decl.source_name.clone();
                        return Ok(CapabilitySource::FilteredProvider(FilteredProviderSource {
                            capability: AggregateCapability::Service(source_name),
                            moniker: moniker,
                            service_capability: capability,
                            offer_service_decl,
                        }));
                    }
                }
            }
            Ok(capability_source)
        }
        OfferResult::OfferFromAnonymizedAggregate(offers, aggregation_component) => {
            let mut members = vec![];
            for o in offers.iter() {
                match o.source() {
                    OfferSource::Collection(n) => {
                        members.push(AggregateMember::Collection(n.clone()));
                    }
                    OfferSource::Child(c) => {
                        assert!(
                            c.collection.is_none(),
                            "Anonymized offer source contained a dynamic child"
                        );
                        members.push(AggregateMember::Child(c.clone()));
                    }
                    OfferSource::Parent => {
                        members.push(AggregateMember::Parent);
                    }
                    OfferSource::Self_ => {
                        members.push(AggregateMember::Self_);
                    }
                    _ => unreachable!("impossible source"),
                }
            }
            let first_offer = offers.iter().next().unwrap();
            Ok(CapabilitySource::AnonymizedAggregate(AnonymizedAggregateSource {
                capability: AggregateCapability::Service(first_offer.source_name().clone()),
                moniker: aggregation_component.moniker().clone(),
                sources: sources.clone(),
                members,
            }))
        }
        OfferResult::OfferFromFilteredAggregate(offers, aggregation_component) => {
            // Check that all of the service offers contain non-conflicting filter instances.
            let mut seen_instances: HashSet<Name> = HashSet::new();
            for o in offers.iter() {
                if let OfferDecl::Service(offer_service_decl) = o.clone().into() {
                    match offer_service_decl.source_instance_filter {
                        None => {
                            return Err(RoutingError::unsupported_route_source(
                                aggregation_component.moniker().clone(),
                                "Aggregate offers must be of service capabilities \
                            with source_instance_filter set",
                            ));
                        }
                        Some(allowed_instances) => {
                            for instance in allowed_instances.iter() {
                                if !seen_instances.insert(instance.clone()) {
                                    return Err(RoutingError::unsupported_route_source(
                                        aggregation_component.moniker().clone(),
                                        format!(
                                            "Instance {} found in multiple offers \
                                                of the same service.",
                                            instance
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                } else {
                    return Err(RoutingError::unsupported_route_source(
                        aggregation_component.moniker().clone(),
                        "Aggregate source must consist of only service capabilities",
                    ));
                }
            }
            // "_unused" is a placeholder value required by fold(). Since `offers` is guaranteed to
            // be nonempty, it will always be overwritten by the actual source name.
            let (source_name, offer_service_decls) = offers.iter().fold(
                ("_unused".parse().unwrap(), Vec::<OfferServiceDecl>::new()),
                |(_, mut decls), o| {
                    if let OfferDecl::Service(offer_service_decl) = o.clone().into() {
                        decls.push(offer_service_decl);
                    }
                    (o.source_name().clone(), decls)
                },
            );
            // TODO(https://fxbug.dev/42151281) Make the Collection CapabilitySource type generic
            // for other types of aggregations.
            Ok(CapabilitySource::FilteredAggregateProvider(FilteredAggregateProviderSource {
                capability: AggregateCapability::Service(source_name),
                moniker: aggregation_component.moniker().clone(),
                offer_service_decls,
                sources: sources.clone(),
            }))
        }
    }
}

/// Routes a capability from its `Expose` declaration to its source by following `Expose`
/// declarations.
///
/// `sources` defines what are the valid sources of the capability. See [`Sources`].
/// `visitor` is invoked for each `Expose` declaration in the routing path, as well as the final
/// `Capability` declaration if `sources` permits.
pub async fn route_from_expose<C, V>(
    expose: RouteBundle<ExposeDecl>,
    expose_target: Arc<C>,
    sources: Sources,
    visitor: &mut V,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<CapabilitySource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
{
    let cap_type = CapabilityTypeName::from(expose.iter().next().unwrap());
    for expose in expose.iter() {
        if !expose.source_path().dirname.is_dot() {
            return Err(RoutingError::DictionariesNotSupported {
                cap_type,
                moniker: expose_target.moniker().clone(),
            });
        }
    }
    match Expose::route(expose, expose_target, &sources, visitor, mapper).await? {
        ExposeResult::Source(source) => Ok(source),
        ExposeResult::ExposeFromAnonymizedAggregate(expose, aggregation_component) => {
            let mut members = vec![];
            for e in expose.iter() {
                match e.source() {
                    ExposeSource::Collection(n) => {
                        members.push(AggregateMember::Collection(n.clone()));
                    }
                    ExposeSource::Child(n) => {
                        members.push(AggregateMember::Child(ChildRef {
                            name: LongName::new(n.clone()).unwrap(),
                            collection: None,
                        }));
                    }
                    ExposeSource::Self_ => {
                        members.push(AggregateMember::Self_);
                    }
                    _ => unreachable!("this was checked before"),
                }
            }
            let first_expose = expose.iter().next().expect("empty bundle");
            Ok(CapabilitySource::AnonymizedAggregate(AnonymizedAggregateSource {
                capability: AggregateCapability::Service(first_expose.source_name().clone()),
                moniker: aggregation_component.moniker().clone(),
                sources: sources.clone(),
                members,
            }))
        }
    }
}

/// Routes a capability from its `Use` declaration to its source by capabilities declarations, i.e.
/// whatever capabilities that this component itself provides.
///
/// `sources` defines what are the valid sources of the capability. See [`Sources`].
/// `visitor` is invoked for each `Capability` declaration if `sources` permits.
pub async fn route_from_self<C, V>(
    use_decl: UseDecl,
    target: Arc<C>,
    sources: Sources,
    visitor: &mut V,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<CapabilitySource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
{
    let cap_type = CapabilityTypeName::from(&use_decl);
    if !use_decl.source_path().dirname.is_dot() {
        return Err(RoutingError::DictionariesNotSupported {
            cap_type,
            moniker: target.moniker().clone(),
        });
    }
    mapper.add_use(target.moniker().clone(), &use_decl.clone().into());
    route_from_self_by_name(use_decl.source_name(), target, sources, visitor, mapper).await
}

/// Routes a capability from a capability name to its source by capabilities declarations, i.e.
/// whatever capabilities that this component itself provides.
///
/// `sources` defines what are the valid sources of the capability. See [`Sources`].
/// `visitor` is invoked for each `Capability` declaration if `sources` permits.
pub async fn route_from_self_by_name<C, V>(
    name: &Name,
    target: Arc<C>,
    sources: Sources,
    visitor: &mut V,
    mapper: &mut dyn DebugRouteMapper,
) -> Result<CapabilitySource, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
{
    let target_capabilities = target.lock_resolved_state().await?.capabilities();
    Ok(CapabilitySource::Component(ComponentSource {
        capability: sources.find_component_source(
            name,
            target.moniker(),
            &target_capabilities,
            visitor,
            mapper,
        )?,
        moniker: target.moniker().clone(),
    }))
}

/// Defines which capability source types are supported.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::Sources")]
pub struct Sources {
    framework: bool,
    builtin: bool,
    capability: bool,
    collection: bool,
    namespace: bool,
    component: bool,
    capability_type: CapabilityTypeName,
}

// Implementation of `Sources` that allows namespace, component, and/or built-in source
// types.
impl Sources {
    /// Creates a new [`Sources`] that does not allow any capability source types.
    pub fn new(capability: CapabilityTypeName) -> Self {
        Self {
            framework: false,
            builtin: false,
            capability: false,
            collection: false,
            namespace: false,
            component: false,
            capability_type: capability,
        }
    }

    /// Allows framework capability sources of the given type (`from: "framework"` in `CML`).
    pub fn framework(self) -> Self {
        Self { framework: true, ..self }
    }

    /// Allows capability source types that originate from other capabilities (`from: "#storage"` in
    /// `CML`).
    pub fn capability(self) -> Self {
        Self { capability: true, ..self }
    }

    /// Allows capability sources to originate from a collection.
    pub fn collection(self) -> Self {
        Self { collection: true, ..self }
    }

    /// Allows namespace capability source types, which are capabilities that are installed in
    /// component_manager's incoming namespace.
    pub fn namespace(self) -> Self {
        Self { namespace: true, ..self }
    }

    /// Allows component capability source types (`from: "self"` in `CML`).
    pub fn component(self) -> Self {
        Self { component: true, ..self }
    }

    /// Allows built-in capability source types (`from: "parent"` in `CML` where the parent component_instance is
    /// component_manager).
    pub fn builtin(self) -> Self {
        Self { builtin: true, ..self }
    }

    /// Return the [`InternalCapability`] representing this framework capability source, or
    /// [`RoutingError::UnsupportedRouteSource`] if unsupported.
    pub fn framework_source(
        &self,
        moniker: &Moniker,
        name: Name,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<InternalCapability, RoutingError> {
        if self.framework {
            let capability = InternalCapability::new(self.capability_type, name.clone());
            mapper.add_framework_capability(name);
            Ok(capability)
        } else {
            Err(RoutingError::unsupported_route_source(moniker.clone(), "framework"))
        }
    }

    /// Checks whether capability sources are supported, returning [`RoutingError::UnsupportedRouteSource`]
    /// if they are not.
    // TODO(https://fxbug.dev/42140194): Add route mapping for capability sources.
    pub fn capability_source(&self, moniker: &Moniker) -> Result<(), RoutingError> {
        if self.capability {
            Ok(())
        } else {
            Err(RoutingError::unsupported_route_source(moniker.clone(), "capability"))
        }
    }

    /// Checks whether namespace capability sources are supported.
    pub fn is_namespace_supported(&self) -> bool {
        self.namespace
    }

    /// Looks for a namespace capability in the list of capability sources.
    /// If found, the declaration is visited by `visitor` and the declaration is wrapped
    /// in a [`ComponentCapability`].
    /// Returns [`RoutingError::UnsupportedRouteSource`] if namespace capabilities are unsupported.
    pub fn find_namespace_source<V>(
        &self,
        moniker: impl Into<ExtendedMoniker>,
        name: &Name,
        capabilities: &[CapabilityDecl],
        visitor: &mut V,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<Option<ComponentCapability>, RoutingError>
    where
        V: CapabilityVisitor,
    {
        if self.namespace {
            if let Some(decl) = capabilities
                .iter()
                .find(|decl: &&CapabilityDecl| {
                    self.capability_type == CapabilityTypeName::from(*decl) && decl.name() == name
                })
                .cloned()
            {
                let moniker = moniker.into();
                visitor.visit(&moniker.into(), &decl)?;
                mapper.add_namespace_capability(&decl);
                Ok(Some(decl.into()))
            } else {
                Ok(None)
            }
        } else {
            Err(RoutingError::unsupported_route_source(moniker, "namespace"))
        }
    }

    /// Looks for a built-in capability in the list of capability sources.
    /// If found, the capability's name is wrapped in an [`InternalCapability`].
    /// Returns [`RoutingError::UnsupportedRouteSource`] if built-in capabilities are unsupported.
    pub fn find_builtin_source<V>(
        &self,
        moniker: impl Into<ExtendedMoniker>,
        name: &Name,
        capabilities: &[CapabilityDecl],
        visitor: &mut V,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<Option<InternalCapability>, RoutingError>
    where
        V: CapabilityVisitor,
    {
        if self.builtin {
            if let Some(decl) = capabilities
                .iter()
                .find(|decl: &&CapabilityDecl| {
                    self.capability_type == CapabilityTypeName::from(*decl) && decl.name() == name
                })
                .cloned()
            {
                visitor.visit(&moniker.into(), &decl)?;
                mapper.add_builtin_capability(&decl);
                Ok(Some(decl.into()))
            } else {
                Ok(None)
            }
        } else {
            Err(RoutingError::unsupported_route_source(moniker, "built-in"))
        }
    }

    /// Looks for a component capability in the list of capability sources for the component instance
    /// with moniker `moniker`.
    /// If found, the declaration is visited by `visitor` and the declaration is wrapped
    /// in a [`ComponentCapability`].
    /// Returns [`RoutingError::UnsupportedRouteSource`] if component capabilities are unsupported.
    pub fn find_component_source<V>(
        &self,
        name: &Name,
        moniker: &Moniker,
        capabilities: &[CapabilityDecl],
        visitor: &mut V,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<ComponentCapability, RoutingError>
    where
        V: CapabilityVisitor,
    {
        if self.component {
            let decl = capabilities
                .iter()
                .find(|decl: &&CapabilityDecl| {
                    self.capability_type == CapabilityTypeName::from(*decl) && decl.name() == name
                })
                .cloned()
                .expect("CapabilityDecl missing, FIDL validation should catch this");
            visitor.visit(&moniker.clone().into(), &decl)?;
            mapper.add_component_capability(moniker.clone(), &decl);
            Ok(decl.into())
        } else {
            Err(RoutingError::unsupported_route_source(moniker.clone(), "component"))
        }
    }
}

pub struct Use();

/// The result of routing a Use declaration to the next phase.
enum UseResult<C: ComponentInstanceInterface + 'static> {
    /// The source of the Use was found (Framework, AboveRoot, etc.)
    Source(CapabilitySource),
    /// The Use led to a parent offer.
    OfferFromParent(RouteBundle<OfferDecl>, Arc<C>),
    /// The Use led to a child Expose declaration.
    /// Note: Instead of FromChild carrying an ExposeDecl of the matching child, it carries a
    /// UseDecl. This is because some RoutingStrategy<> don't support Expose, but are still
    /// required to enumerate over UseResult<>.
    ExposeFromChild(UseDecl, Arc<C>),
}

impl Use {
    /// Routes the capability starting from the `use_` declaration at `target` to either a valid
    /// source (as defined by `sources`) or the Offer declaration that ends this phase of routing.
    async fn route<C, V>(
        use_: UseDecl,
        target: Arc<C>,
        sources: &Sources,
        visitor: &mut V,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<UseResult<C>, RoutingError>
    where
        C: ComponentInstanceInterface + 'static,
        V: CapabilityVisitor,
    {
        mapper.add_use(target.moniker().clone(), &use_);
        match use_.source() {
            UseSource::Framework => {
                Ok(UseResult::Source(CapabilitySource::Framework(FrameworkSource {
                    capability: sources.framework_source(
                        target.moniker(),
                        use_.source_name().clone(),
                        mapper,
                    )?,
                    moniker: target.moniker().clone(),
                })))
            }
            UseSource::Capability(_) => {
                sources.capability_source(target.moniker())?;
                Ok(UseResult::Source(CapabilitySource::Capability(CapabilityToCapabilitySource {
                    moniker: target.moniker().clone(),
                    source_capability: ComponentCapability::Use_(use_.into()),
                })))
            }
            UseSource::Parent => match target.try_get_parent()? {
                ExtendedInstanceInterface::<C>::AboveRoot(top_instance) => {
                    if sources.is_namespace_supported() {
                        if let Some(capability) = sources.find_namespace_source(
                            ExtendedMoniker::ComponentManager,
                            use_.source_name(),
                            top_instance.namespace_capabilities(),
                            visitor,
                            mapper,
                        )? {
                            return Ok(UseResult::Source(CapabilitySource::Namespace(
                                NamespaceSource { capability },
                            )));
                        }
                    }
                    if let Some(capability) = sources.find_builtin_source(
                        ExtendedMoniker::ComponentManager,
                        use_.source_name(),
                        top_instance.builtin_capabilities(),
                        visitor,
                        mapper,
                    )? {
                        return Ok(UseResult::Source(CapabilitySource::Builtin(BuiltinSource {
                            capability,
                        })));
                    }
                    Err(RoutingError::use_from_component_manager_not_found(
                        use_.source_name().to_string(),
                    ))
                }
                ExtendedInstanceInterface::<C>::Component(parent_component) => {
                    let parent_offers = parent_component.lock_resolved_state().await?.offers();
                    let child_moniker = target.child_moniker().expect("ChildName should exist");
                    let parent_offers = find_matching_offers(
                        CapabilityTypeName::from(&use_),
                        use_.source_name(),
                        &child_moniker,
                        &parent_offers,
                    )
                    .ok_or_else(|| {
                        <UseDecl as ErrorNotFoundFromParent>::error_not_found_from_parent(
                            target.moniker().clone(),
                            use_.source_name().clone(),
                        )
                    })?;
                    Ok(UseResult::OfferFromParent(parent_offers, parent_component))
                }
            },
            UseSource::Child(name) => {
                let moniker = target.moniker();
                let child_component = {
                    let child_moniker = ChildName::new(name.clone().into(), None);
                    target.lock_resolved_state().await?.get_child(&child_moniker).ok_or_else(
                        || {
                            RoutingError::use_from_child_instance_not_found(
                                &child_moniker,
                                moniker,
                                name.clone(),
                            )
                        },
                    )?
                };
                Ok(UseResult::ExposeFromChild(use_, child_component))
            }
            UseSource::Collection(name) => Ok(UseResult::Source(
                CapabilitySource::AnonymizedAggregate(AnonymizedAggregateSource {
                    capability: AggregateCapability::Service(use_.source_name().clone()),
                    moniker: target.moniker().clone(),
                    sources: sources.clone(),
                    members: vec![AggregateMember::Collection(name.clone())],
                }),
            )),
            UseSource::Debug => {
                // This is not supported today. It might be worthwhile to support this if
                // more than just protocol has a debug capability.
                return Err(RoutingError::unsupported_route_source(
                    target.moniker().clone(),
                    "debug capability",
                ));
            }
            UseSource::Self_ => {
                let use_: UseDecl = use_.into();
                if let UseDecl::Config(config) = use_ {
                    sources.capability_source(target.moniker())?;
                    let target_capabilities = target.lock_resolved_state().await?.capabilities();
                    return Ok(UseResult::Source(CapabilitySource::Component(ComponentSource {
                        capability: sources.find_component_source(
                            config.source_name(),
                            target.moniker(),
                            &target_capabilities,
                            visitor,
                            mapper,
                        )?,
                        moniker: target.moniker().clone(),
                    })));
                }
                return Err(RoutingError::unsupported_route_source(
                    target.moniker().clone(),
                    "self",
                ));
            }
            UseSource::Environment => {
                // This is not supported today. It might be worthwhile to support this if
                // capabilities other than runner can be used from environment.
                return Err(RoutingError::unsupported_route_source(
                    target.moniker().clone(),
                    "environment",
                ));
            }
        }
    }
}

/// The environment `Registration` phase of routing.
pub struct Registration<R>(PhantomData<R>);

/// The result of routing a Registration declaration to the next phase.
enum RegistrationResult<C: ComponentInstanceInterface + 'static, O: Clone + fmt::Debug> {
    /// The source of the Registration was found (Framework, AboveRoot, etc.).
    Source(CapabilitySource),
    /// The Registration led to a parent Offer declaration.
    FromParent(RouteBundle<O>, Arc<C>),
    /// The Registration led to a child Expose declaration.
    FromChild(RouteBundle<ExposeDecl>, Arc<C>),
}

impl<R> Registration<R>
where
    R: RegistrationDeclCommon
        + ErrorNotFoundFromParent
        + ErrorNotFoundInChild
        + Into<RegistrationDecl>
        + Clone,
{
    /// Routes the capability starting from the `registration` declaration at `target` to either a
    /// valid source (as defined by `sources`) or the Offer or Expose declaration that ends this
    /// phase of routing.
    async fn route<C, V>(
        registration: R,
        target: Arc<C>,
        sources: &Sources,
        visitor: &mut V,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<RegistrationResult<C, OfferDecl>, RoutingError>
    where
        C: ComponentInstanceInterface + 'static,
        V: CapabilityVisitor,
    {
        let registration_decl: RegistrationDecl = registration.clone().into();
        mapper.add_registration(target.moniker().clone(), &registration_decl);
        match registration.source() {
            RegistrationSource::Self_ => {
                let target_capabilities = target.lock_resolved_state().await?.capabilities();
                Ok(RegistrationResult::Source(CapabilitySource::Component(ComponentSource {
                    capability: sources.find_component_source(
                        registration.source_name(),
                        target.moniker(),
                        &target_capabilities,
                        visitor,
                        mapper,
                    )?,
                    moniker: target.moniker().clone(),
                })))
            }
            RegistrationSource::Parent => match target.try_get_parent()? {
                ExtendedInstanceInterface::<C>::AboveRoot(top_instance) => {
                    if sources.is_namespace_supported() {
                        if let Some(capability) = sources.find_namespace_source(
                            ExtendedMoniker::ComponentManager,
                            registration.source_name(),
                            top_instance.namespace_capabilities(),
                            visitor,
                            mapper,
                        )? {
                            return Ok(RegistrationResult::Source(CapabilitySource::Namespace(
                                NamespaceSource { capability },
                            )));
                        }
                    }
                    if let Some(capability) = sources.find_builtin_source(
                        ExtendedMoniker::ComponentManager,
                        registration.source_name(),
                        top_instance.builtin_capabilities(),
                        visitor,
                        mapper,
                    )? {
                        return Ok(RegistrationResult::Source(CapabilitySource::Builtin(
                            BuiltinSource { capability },
                        )));
                    }
                    Err(RoutingError::register_from_component_manager_not_found(
                        registration.source_name().to_string(),
                    ))
                }
                ExtendedInstanceInterface::<C>::Component(parent_component) => {
                    let parent_offers = parent_component.lock_resolved_state().await?.offers();
                    let child_moniker = target.child_moniker().expect("ChildName should exist");
                    let parent_offers = find_matching_offers(
                        CapabilityTypeName::from(&registration_decl),
                        registration.source_name(),
                        &child_moniker,
                        &parent_offers,
                    )
                    .ok_or_else(|| {
                        <R as ErrorNotFoundFromParent>::error_not_found_from_parent(
                            target.moniker().clone(),
                            registration.source_name().clone(),
                        )
                    })?;
                    Ok(RegistrationResult::FromParent(parent_offers, parent_component))
                }
            },
            RegistrationSource::Child(child) => {
                let child_component = {
                    let child_moniker = ChildName::try_new(child, None).expect(
                        "discovered invalid child name, manifest validation should prevent this",
                    );
                    target.lock_resolved_state().await?.get_child(&child_moniker).ok_or_else(
                        || RoutingError::EnvironmentFromChildInstanceNotFound {
                            child_moniker,
                            moniker: target.moniker().clone(),
                            capability_name: registration.source_name().clone(),
                            capability_type: R::TYPE.to_string(),
                        },
                    )?
                };

                let child_exposes = child_component.lock_resolved_state().await?.exposes();
                let capability_type = CapabilityTypeName::from(&registration.clone().into());
                let child_exposes = find_matching_exposes(
                    capability_type,
                    registration.source_name(),
                    &child_exposes,
                )
                .ok_or_else(|| {
                    let child_moniker =
                        child_component.child_moniker().expect("ChildName should exist");
                    <R as ErrorNotFoundInChild>::error_not_found_in_child(
                        target.moniker().clone(),
                        child_moniker.clone(),
                        registration.source_name().clone(),
                    )
                })?;
                Ok(RegistrationResult::FromChild(child_exposes, child_component.clone()))
            }
        }
    }
}

/// The `Offer` phase of routing.
pub struct Offer();

/// The result of routing an Offer declaration to the next phase.
enum OfferResult<C: ComponentInstanceInterface + 'static> {
    /// The source of the Offer was found (Framework, AboveRoot, Component, etc.).
    Source(CapabilitySource),
    /// The Offer led to an Offer-from-child declaration.
    /// Not all capabilities can be exposed, so let the caller decide how to handle this.
    OfferFromChild(OfferDecl, Arc<C>),
    /// Offer from multiple static children, with filters.
    OfferFromFilteredAggregate(RouteBundle<OfferDecl>, Arc<C>),
    /// Offer from one or more collections and/or static children.
    OfferFromAnonymizedAggregate(RouteBundle<OfferDecl>, Arc<C>),
}

enum OfferSegment<C: ComponentInstanceInterface + 'static> {
    Done(OfferResult<C>),
    Next(RouteBundle<OfferDecl>, Arc<C>),
}

impl Offer {
    /// Routes the capability starting from the `offer` declaration at `target` to either a valid
    /// source (as defined by `sources`) or the declaration that ends this phase of routing.
    async fn route<C, V>(
        mut offer_bundle: RouteBundle<OfferDecl>,
        mut target: Arc<C>,
        sources: &Sources,
        visitor: &mut V,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<OfferResult<C>, RoutingError>
    where
        C: ComponentInstanceInterface + 'static,
        V: OfferVisitor,
        V: CapabilityVisitor,
    {
        loop {
            let visit_offer = match &offer_bundle {
                RouteBundle::Single(offer) => Some(offer),
                RouteBundle::Aggregate(offers) => {
                    // TODO(https://fxbug.dev/42124541): Visit routes in all aggregates.
                    if offers.len() == 1 {
                        Some(&offers[0])
                    } else {
                        None
                    }
                }
            };
            if let Some(visit_offer) = visit_offer {
                mapper.add_offer(target.moniker().clone(), &visit_offer);
                OfferVisitor::visit(visitor, &target.moniker().clone().into(), &visit_offer)?;
            }

            fn is_filtered_offer(o: &OfferDecl) -> bool {
                if let OfferDecl::Service(offer_service) = o {
                    if let Some(f) = offer_service.source_instance_filter.as_ref() {
                        if !f.is_empty() {
                            return true;
                        }
                    }
                    if let Some(f) = offer_service.renamed_instances.as_ref() {
                        if !f.is_empty() {
                            return true;
                        }
                    }
                }
                false
            }

            // Make sure this isn't coming from a dictionary
            for o in offer_bundle.iter() {
                if !o.source_path().dirname.is_dot() {
                    return Err(RoutingError::DictionariesNotSupported {
                        moniker: target.moniker().clone(),
                        cap_type: CapabilityTypeName::from(o),
                    });
                }
            }

            match offer_bundle {
                RouteBundle::Single(offer) => {
                    match Self::route_segment(offer, target, sources, visitor, mapper).await? {
                        OfferSegment::Done(r) => return Ok(r),
                        OfferSegment::Next(o, t) => {
                            offer_bundle = o;
                            target = t;
                        }
                    }
                }
                RouteBundle::Aggregate(_) => {
                    if offer_bundle.iter().all(|o| !is_filtered_offer(o)) {
                        return Ok(OfferResult::OfferFromAnonymizedAggregate(offer_bundle, target));
                    } else {
                        return Ok(OfferResult::OfferFromFilteredAggregate(offer_bundle, target));
                    }
                }
            }
        }
    }

    async fn route_segment<C, V>(
        offer: OfferDecl,
        target: Arc<C>,
        sources: &Sources,
        visitor: &mut V,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<OfferSegment<C>, RoutingError>
    where
        C: ComponentInstanceInterface + 'static,
        V: OfferVisitor,
        V: CapabilityVisitor,
    {
        let res = match offer.source() {
            OfferSource::Void => {
                OfferSegment::Done(OfferResult::Source(CapabilitySource::Void(VoidSource {
                    capability: InternalCapability::new(
                        (&offer).into(),
                        offer.source_name().clone(),
                    ),
                    moniker: target.moniker().clone(),
                })))
            }
            OfferSource::Self_ => {
                let target_capabilities = target.lock_resolved_state().await?.capabilities();
                let capability = sources.find_component_source(
                    offer.source_name(),
                    target.moniker(),
                    &target_capabilities,
                    visitor,
                    mapper,
                )?;
                // if offerdecl is for a filtered service return the associated filtered source.
                let component = target.as_weak();
                let moniker = target.moniker().clone();
                let res = match offer.into() {
                    OfferDecl::Service(offer_service_decl) => {
                        if offer_service_decl.source_instance_filter.is_some()
                            || offer_service_decl.renamed_instances.is_some()
                        {
                            let source_name = offer_service_decl.source_name.clone();
                            OfferResult::Source(CapabilitySource::FilteredProvider(
                                FilteredProviderSource {
                                    capability: AggregateCapability::Service(source_name),
                                    moniker,
                                    service_capability: capability,
                                    offer_service_decl,
                                },
                            ))
                        } else {
                            OfferResult::Source(CapabilitySource::Component(ComponentSource {
                                capability,
                                moniker: component.moniker,
                            }))
                        }
                    }
                    _ => OfferResult::Source(CapabilitySource::Component(ComponentSource {
                        capability,
                        moniker: component.moniker,
                    })),
                };
                OfferSegment::Done(res)
            }
            OfferSource::Framework => OfferSegment::Done(OfferResult::Source(
                CapabilitySource::Framework(FrameworkSource {
                    capability: sources.framework_source(
                        target.moniker(),
                        offer.source_name().clone(),
                        mapper,
                    )?,
                    moniker: target.moniker().clone(),
                }),
            )),
            OfferSource::Capability(_) => {
                sources.capability_source(target.moniker())?;
                OfferSegment::Done(OfferResult::Source(CapabilitySource::Capability(
                    CapabilityToCapabilitySource {
                        source_capability: ComponentCapability::Offer(offer.into()),
                        moniker: target.moniker().clone(),
                    },
                )))
            }
            OfferSource::Parent => {
                let parent_component = match target.try_get_parent()? {
                    ExtendedInstanceInterface::<C>::AboveRoot(top_instance) => {
                        if sources.is_namespace_supported() {
                            if let Some(capability) = sources.find_namespace_source(
                                ExtendedMoniker::ComponentManager,
                                offer.source_name(),
                                top_instance.namespace_capabilities(),
                                visitor,
                                mapper,
                            )? {
                                return Ok(OfferSegment::Done(OfferResult::Source(
                                    CapabilitySource::Namespace(NamespaceSource { capability }),
                                )));
                            }
                        }
                        if let Some(capability) = sources.find_builtin_source(
                            ExtendedMoniker::ComponentManager,
                            offer.source_name(),
                            top_instance.builtin_capabilities(),
                            visitor,
                            mapper,
                        )? {
                            return Ok(OfferSegment::Done(OfferResult::Source(
                                CapabilitySource::Builtin(BuiltinSource { capability }),
                            )));
                        }
                        return Err(RoutingError::offer_from_component_manager_not_found(
                            offer.source_name().to_string(),
                        ));
                    }
                    ExtendedInstanceInterface::<C>::Component(component) => component,
                };
                let child_moniker = target.child_moniker().expect("ChildName should exist");
                let parent_offers = parent_component.lock_resolved_state().await?.offers();
                let parent_offers = find_matching_offers(
                    CapabilityTypeName::from(&offer),
                    offer.source_name(),
                    &child_moniker,
                    &parent_offers,
                )
                .ok_or_else(|| {
                    <OfferDecl as ErrorNotFoundFromParent>::error_not_found_from_parent(
                        target.moniker().clone(),
                        offer.source_name().clone(),
                    )
                })?;
                OfferSegment::Next(parent_offers, parent_component)
            }
            OfferSource::Child(_) => OfferSegment::Done(OfferResult::OfferFromChild(offer, target)),
            OfferSource::Collection(_) => OfferSegment::Done(
                OfferResult::OfferFromAnonymizedAggregate(RouteBundle::from_offer(offer), target),
            ),
        };
        Ok(res)
    }
}

/// Finds the matching Expose declaration for an Offer-from-child, changing the
/// direction in which the Component Tree is being navigated (from up to down).
async fn change_directions<C>(
    offer: OfferDecl,
    component: Arc<C>,
) -> Result<(RouteBundle<ExposeDecl>, Arc<C>), RoutingError>
where
    C: ComponentInstanceInterface,
{
    match offer.source() {
        OfferSource::Child(child) => {
            let child_component = {
                let child_moniker = ChildName::try_new(
                    child.name.as_str(),
                    child.collection.as_ref().map(|s| s.as_str()),
                )
                .expect("discovered invalid child name, manifest validation should prevent this");
                component.lock_resolved_state().await?.get_child(&child_moniker).ok_or_else(
                    || RoutingError::OfferFromChildInstanceNotFound {
                        child_moniker,
                        moniker: component.moniker().clone(),
                        capability_id: offer.source_name().clone().into(),
                    },
                )?
            };
            let child_exposes = child_component.lock_resolved_state().await?.exposes();
            let child_exposes = find_matching_exposes(
                CapabilityTypeName::from(&offer),
                offer.source_name(),
                &child_exposes,
            )
            .ok_or_else(|| {
                let child_moniker =
                    child_component.child_moniker().expect("ChildName should exist");
                <OfferDecl as ErrorNotFoundInChild>::error_not_found_in_child(
                    component.moniker().clone(),
                    child_moniker.clone(),
                    offer.source_name().clone(),
                )
            })?;
            Ok((child_exposes, child_component.clone()))
        }
        _ => panic!("change_direction called with offer that does not change direction"),
    }
}

/// The `Expose` phase of routing.
#[derive(Debug)]
pub struct Expose();

/// The result of routing an Expose declaration to the next phase.
enum ExposeResult<C: ComponentInstanceInterface + 'static> {
    /// The source of the Expose was found (Framework, Component, etc.).
    Source(CapabilitySource),
    /// The source of the Expose comes from an aggregation of collections and/or static children
    ExposeFromAnonymizedAggregate(RouteBundle<ExposeDecl>, Arc<C>),
}

/// A bundle of one or more routing declarations to route together, that share the same target_name
#[derive(Clone, Debug)]
pub enum RouteBundle<T>
where
    T: Clone + fmt::Debug,
{
    /// A single route from a unique source.
    Single(T),
    /// A bundle of routes representing an aggregated capability. This can be a vector of one,
    /// e.g. exposing a service from a collection.
    Aggregate(Vec<T>),
}

impl<T> RouteBundle<T>
where
    T: Clone + fmt::Debug,
{
    pub fn map<U: Clone + fmt::Debug>(self, mut f: impl FnMut(T) -> U) -> RouteBundle<U> {
        match self {
            RouteBundle::Single(r) => RouteBundle::Single(f(r)),
            RouteBundle::Aggregate(r) => {
                RouteBundle::Aggregate(r.into_iter().map(&mut f).collect())
            }
        }
    }

    /// Returns an iterator over the values of `OneOrMany<T>`.
    pub fn iter(&self) -> RouteBundleIter<'_, T> {
        match self {
            Self::Single(item) => {
                RouteBundleIter { inner_single: Some(item), inner_aggregate: None }
            }
            Self::Aggregate(items) => {
                RouteBundleIter { inner_single: None, inner_aggregate: Some(items.iter()) }
            }
        }
    }

    /// Returns the number of routes in this bundle.
    pub fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Aggregate(v) => v.len(),
        }
    }

    /// Creates a `RouteBundle` from of a single offer routing declaration.
    pub fn from_offer(input: T) -> Self
    where
        T: OfferDeclCommon + Clone,
    {
        Self::from_offers(vec![input])
    }

    /// Creates a `RouteBundle` from of a list of offer routing declarations.
    ///
    /// REQUIRES: `input` is nonempty.
    /// REQUIRES: All elements of `input` share the same `target_name`.
    pub fn from_offers(mut input: Vec<T>) -> Self
    where
        T: OfferDeclCommon + Clone,
    {
        match input.len() {
            1 => {
                let input = input.remove(0);
                match input.source() {
                    OfferSource::Collection(_) => Self::Aggregate(vec![input]),
                    _ => Self::Single(input),
                }
            }
            0 => panic!("empty bundles are not allowed"),
            _ => Self::Aggregate(input),
        }
    }

    /// Creates a `RouteBundle` from of a single expose routing declaration.
    pub fn from_expose(input: T) -> Self
    where
        T: ExposeDeclCommon + Clone,
    {
        Self::from_exposes(vec![input])
    }

    /// Creates a `RouteBundle` from of a list of expose routing declarations.
    ///
    /// REQUIRES: `input` is nonempty.
    /// REQUIRES: All elements of `input` share the same `target_name`.
    pub fn from_exposes(mut input: Vec<T>) -> Self
    where
        T: ExposeDeclCommon + Clone,
    {
        match input.len() {
            1 => {
                let input = input.remove(0);
                match input.source() {
                    ExposeSource::Collection(_) => Self::Aggregate(vec![input]),
                    _ => Self::Single(input),
                }
            }
            0 => panic!("empty bundles are not allowed"),
            _ => Self::Aggregate(input),
        }
    }
}

impl<T> RouteBundle<T>
where
    T: ExposeDeclCommon + Clone,
{
    pub fn availability(&self) -> &Availability {
        match self {
            Self::Single(e) => e.availability(),
            Self::Aggregate(v) => {
                assert!(
                    v.iter().zip(v.iter().skip(1)).all(|(a, b)| a.availability() == b.availability()),
                    "CM validation should ensure all aggregated capabilities have the same availability");
                v[0].availability()
            }
        }
    }
}

/// Immutable iterator over a `RouteBundle`.
/// This `struct` is created by [`RouteBundle::iter`].
///
/// [`RouteBundle::iter`]: struct.RouteBundle.html#method.iter
pub struct RouteBundleIter<'a, T> {
    inner_single: Option<&'a T>,
    inner_aggregate: Option<slice::Iter<'a, T>>,
}

impl<'a, T> Iterator for RouteBundleIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(item) = self.inner_single.take() {
            Some(item)
        } else if let Some(ref mut iter) = &mut self.inner_aggregate {
            iter.next()
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if let Some(_) = self.inner_single {
            (1, Some(1))
        } else if let Some(iter) = &self.inner_aggregate {
            iter.size_hint()
        } else {
            (0, Some(0))
        }
    }
}

impl<'a, T> ExactSizeIterator for RouteBundleIter<'a, T> {}

enum ExposeSegment<C: ComponentInstanceInterface + 'static> {
    Done(ExposeResult<C>),
    Next(RouteBundle<ExposeDecl>, Arc<C>),
}

impl Expose {
    /// Routes the capability starting from the `expose` declaration at `target` to a valid source
    /// (as defined by `sources`).
    async fn route<C, V>(
        mut expose_bundle: RouteBundle<ExposeDecl>,
        mut target: Arc<C>,
        sources: &Sources,
        visitor: &mut V,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<ExposeResult<C>, RoutingError>
    where
        C: ComponentInstanceInterface + 'static,
        V: ExposeVisitor,
        V: CapabilityVisitor,
    {
        loop {
            let visit_expose = match &expose_bundle {
                RouteBundle::Single(expose) => Some(expose),
                RouteBundle::Aggregate(exposes) => {
                    // TODO(https://fxbug.dev/42124541): Visit routes in all aggregates.
                    if exposes.len() == 1 {
                        Some(&exposes[0])
                    } else {
                        None
                    }
                }
            };
            if let Some(visit_expose) = visit_expose {
                mapper.add_expose(target.moniker().clone(), visit_expose.clone().into());
                ExposeVisitor::visit(visitor, &target.moniker().clone().into(), &visit_expose)?;
            }

            // Make sure this isn't coming from a dictionary
            for e in expose_bundle.iter() {
                if !e.source_path().dirname.is_dot() {
                    return Err(RoutingError::DictionariesNotSupported {
                        moniker: target.moniker().clone(),
                        cap_type: CapabilityTypeName::from(e),
                    });
                }
            }

            match expose_bundle {
                RouteBundle::Single(expose) => {
                    match Self::route_segment(expose, target, sources, visitor, mapper).await? {
                        ExposeSegment::Done(r) => return Ok(r),
                        ExposeSegment::Next(e, t) => {
                            expose_bundle = e;
                            target = t;
                        }
                    }
                }
                RouteBundle::Aggregate(_) => {
                    return Ok(ExposeResult::ExposeFromAnonymizedAggregate(expose_bundle, target));
                }
            }
        }
    }

    async fn route_segment<C, V>(
        expose: ExposeDecl,
        target: Arc<C>,
        sources: &Sources,
        visitor: &mut V,
        mapper: &mut dyn DebugRouteMapper,
    ) -> Result<ExposeSegment<C>, RoutingError>
    where
        C: ComponentInstanceInterface + 'static,
        V: ExposeVisitor,
        V: CapabilityVisitor,
    {
        let res = match expose.source() {
            ExposeSource::Void => {
                ExposeSegment::Done(ExposeResult::Source(CapabilitySource::Void(VoidSource {
                    capability: InternalCapability::new(
                        (&expose).into(),
                        expose.source_name().clone(),
                    ),
                    moniker: target.moniker().clone(),
                })))
            }
            ExposeSource::Self_ => {
                let target_capabilities = target.lock_resolved_state().await?.capabilities();
                ExposeSegment::Done(ExposeResult::Source(CapabilitySource::Component(
                    ComponentSource {
                        capability: sources.find_component_source(
                            expose.source_name(),
                            target.moniker(),
                            &target_capabilities,
                            visitor,
                            mapper,
                        )?,
                        moniker: target.moniker().clone(),
                    },
                )))
            }
            ExposeSource::Framework => ExposeSegment::Done(ExposeResult::Source(
                CapabilitySource::Framework(FrameworkSource {
                    capability: sources.framework_source(
                        target.moniker(),
                        expose.source_name().clone(),
                        mapper,
                    )?,
                    moniker: target.moniker().clone(),
                }),
            )),
            ExposeSource::Capability(_) => {
                sources.capability_source(target.moniker())?;
                ExposeSegment::Done(ExposeResult::Source(CapabilitySource::Capability(
                    CapabilityToCapabilitySource {
                        source_capability: ComponentCapability::Expose(expose.into()),
                        moniker: target.moniker().clone(),
                    },
                )))
            }
            ExposeSource::Child(child) => {
                let child_component = {
                    let child_moniker = ChildName::new(child.clone().into(), None);
                    target.lock_resolved_state().await?.get_child(&child_moniker).ok_or_else(
                        || RoutingError::ExposeFromChildInstanceNotFound {
                            child_moniker,
                            moniker: target.moniker().clone(),
                            capability_id: expose.source_name().clone().into(),
                        },
                    )?
                };
                let child_exposes = child_component.lock_resolved_state().await?.exposes();
                let child_exposes = find_matching_exposes(
                    CapabilityTypeName::from(&expose),
                    expose.source_name(),
                    &child_exposes,
                )
                .ok_or_else(|| {
                    let child_moniker =
                        child_component.child_moniker().expect("ChildName should exist");
                    <ExposeDecl as ErrorNotFoundInChild>::error_not_found_in_child(
                        target.moniker().clone(),
                        child_moniker.clone(),
                        expose.source_name().clone(),
                    )
                })?;
                ExposeSegment::Next(child_exposes, child_component)
            }
            ExposeSource::Collection(_) => {
                ExposeSegment::Done(ExposeResult::ExposeFromAnonymizedAggregate(
                    RouteBundle::from_expose(expose),
                    target,
                ))
            }
        };
        Ok(res)
    }
}

fn target_matches_moniker(target: &OfferTarget, child_moniker: &ChildName) -> bool {
    match target {
        OfferTarget::Child(target_ref) => {
            &target_ref.name == child_moniker.name()
                && target_ref.collection.as_ref() == child_moniker.collection()
        }
        OfferTarget::Collection(target_collection) => {
            Some(target_collection) == child_moniker.collection()
        }
        OfferTarget::Capability(_) => false,
    }
}

/// Visitor pattern trait for visiting all [`OfferDecl`] during a route.
pub trait OfferVisitor {
    fn visit(&mut self, moniker: &ExtendedMoniker, offer: &OfferDecl) -> Result<(), RoutingError>;
}

/// Visitor pattern trait for visiting all [`ExposeDecl`] during a route.
pub trait ExposeVisitor {
    /// Visit each [`ExposeDecl`] on the route.
    /// Returning an `Err` cancels visitation.
    fn visit(&mut self, moniker: &ExtendedMoniker, expose: &ExposeDecl)
        -> Result<(), RoutingError>;
}

/// Visitor pattern trait for visiting all [`CapabilityDecl`] during a route.
pub trait CapabilityVisitor {
    /// Visit each [`CapabilityDecl`] on the route.
    /// Returning an `Err` cancels visitation.
    fn visit(
        &mut self,
        moniker: &ExtendedMoniker,
        capability: &CapabilityDecl,
    ) -> Result<(), RoutingError>;
}

pub fn find_matching_offers(
    capability_type: CapabilityTypeName,
    source_name: &Name,
    child_moniker: &ChildName,
    offers: &Vec<OfferDecl>,
) -> Option<RouteBundle<OfferDecl>> {
    let offers: Vec<_> = offers
        .iter()
        .filter(|offer: &&OfferDecl| {
            capability_type == CapabilityTypeName::from(*offer)
                && *offer.target_name() == *source_name
                && target_matches_moniker(offer.target(), &child_moniker)
        })
        .cloned()
        .collect();
    if offers.is_empty() {
        return None;
    }
    Some(RouteBundle::from_offers(offers))
}

pub fn find_matching_exposes(
    capability_type: CapabilityTypeName,
    source_name: &Name,
    exposes: &Vec<ExposeDecl>,
) -> Option<RouteBundle<ExposeDecl>> {
    let exposes: Vec<_> = exposes
        .iter()
        .filter(|expose: &&ExposeDecl| {
            capability_type == CapabilityTypeName::from(*expose)
                && *expose.target_name() == *source_name
                && *expose.target() == ExposeTarget::Parent
        })
        .cloned()
        .collect();
    if exposes.is_empty() {
        return None;
    }
    Some(RouteBundle::from_exposes(exposes))
}

/// Implemented by declaration types to emit a proper error when a matching offer is not found in the parent.
pub trait ErrorNotFoundFromParent {
    fn error_not_found_from_parent(
        decl_site_moniker: Moniker,
        capability_name: Name,
    ) -> RoutingError;
}

/// Implemented by declaration types to emit a proper error when a matching expose is not found in the child.
pub trait ErrorNotFoundInChild {
    fn error_not_found_in_child(
        decl_site_moniker: Moniker,
        child_moniker: ChildName,
        capability_name: Name,
    ) -> RoutingError;
}

#[derive(Clone)]
pub struct NoopVisitor {}

impl NoopVisitor {
    pub fn new() -> NoopVisitor {
        NoopVisitor {}
    }
}

impl OfferVisitor for NoopVisitor {
    fn visit(&mut self, _: &ExtendedMoniker, _: &OfferDecl) -> Result<(), RoutingError> {
        Ok(())
    }
}

impl ExposeVisitor for NoopVisitor {
    fn visit(&mut self, _: &ExtendedMoniker, _: &ExposeDecl) -> Result<(), RoutingError> {
        Ok(())
    }
}

impl CapabilityVisitor for NoopVisitor {
    fn visit(&mut self, _: &ExtendedMoniker, _: &CapabilityDecl) -> Result<(), RoutingError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use cm_rust::ExposeServiceDecl;
    use cm_rust_testing::*;

    #[test]
    fn route_bundle_iter_single() {
        let v = RouteBundle::Single(34);
        let mut iter = v.iter();
        assert_matches!(iter.next(), Some(&34));
        assert_matches!(iter.next(), None);
    }

    #[test]
    fn route_bundle_iter_many() {
        let v = RouteBundle::Aggregate(vec![1, 2, 3]);
        let mut iter = v.iter();
        assert_matches!(iter.next(), Some(&1));
        assert_matches!(iter.next(), Some(&2));
        assert_matches!(iter.next(), Some(&3));
        assert_matches!(iter.next(), None);
    }

    #[test]
    fn route_bundle_from_offers() {
        let parent_offers: Vec<_> = [1, 2, 3]
            .into_iter()
            .map(|i| OfferServiceDecl {
                source: OfferSource::Parent,
                source_name: format!("foo_source_{}", i).parse().unwrap(),
                source_dictionary: Default::default(),
                target: OfferTarget::Collection("coll".parse().unwrap()),
                target_name: "foo_target".parse().unwrap(),
                source_instance_filter: None,
                renamed_instances: None,
                availability: Availability::Required,
                dependency_type: Default::default(),
            })
            .collect();
        let collection_offer = OfferServiceDecl {
            source: OfferSource::Collection("coll".parse().unwrap()),
            source_name: "foo_source".parse().unwrap(),
            source_dictionary: Default::default(),
            target: offer_target_static_child("target"),
            target_name: "foo_target".parse().unwrap(),
            source_instance_filter: None,
            renamed_instances: None,
            availability: Availability::Required,
            dependency_type: Default::default(),
        };
        assert_matches!(
            RouteBundle::from_offer(parent_offers[0].clone()),
            RouteBundle::Single(o) if o == parent_offers[0]
        );
        assert_matches!(
            RouteBundle::from_offers(vec![parent_offers[0].clone()]),
            RouteBundle::Single(o) if o == parent_offers[0]
        );
        assert_matches!(
            RouteBundle::from_offers(parent_offers.clone()),
            RouteBundle::Aggregate(v) if v == parent_offers
        );
        assert_matches!(
            RouteBundle::from_offer(collection_offer.clone()),
            RouteBundle::Aggregate(v) if v == vec![collection_offer.clone()]
        );
    }

    #[test]
    fn route_bundle_from_exposes() {
        let child_exposes: Vec<_> = [1, 2, 3]
            .into_iter()
            .map(|i| ExposeServiceDecl {
                source: ExposeSource::Child("source".parse().unwrap()),
                source_name: format!("foo_source_{}", i).parse().unwrap(),
                source_dictionary: Default::default(),
                target: ExposeTarget::Parent,
                target_name: "foo_target".parse().unwrap(),
                availability: Availability::Required,
            })
            .collect();
        let collection_expose = ExposeServiceDecl {
            source: ExposeSource::Collection("coll".parse().unwrap()),
            source_name: "foo_source".parse().unwrap(),
            source_dictionary: Default::default(),
            target: ExposeTarget::Parent,
            target_name: "foo_target".parse().unwrap(),
            availability: Availability::Required,
        };
        assert_matches!(
            RouteBundle::from_expose(child_exposes[0].clone()),
            RouteBundle::Single(o) if o == child_exposes[0]
        );
        assert_matches!(
            RouteBundle::from_exposes(vec![child_exposes[0].clone()]),
            RouteBundle::Single(o) if o == child_exposes[0]
        );
        assert_matches!(
            RouteBundle::from_exposes(child_exposes.clone()),
            RouteBundle::Aggregate(v) if v == child_exposes
        );
        assert_matches!(
            RouteBundle::from_expose(collection_expose.clone()),
            RouteBundle::Aggregate(v) if v == vec![collection_expose.clone()]
        );
    }
}
