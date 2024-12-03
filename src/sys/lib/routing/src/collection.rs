// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability_source::{
    AggregateInstance, AggregateMember, AnonymizedAggregateSource, CapabilitySource,
    ComponentCapability, ComponentSource, FilteredAggregateCapabilityProvider,
    FilteredAggregateCapabilityRouteData, FilteredAggregateProviderSource, FilteredProviderSource,
};
use crate::component_instance::{
    ComponentInstanceInterface, ExtendedInstanceInterface, ResolvedInstanceInterface,
    WeakComponentInstanceInterface,
};
use crate::error::RoutingError;
use crate::legacy_router::{
    self, CapabilityVisitor, ErrorNotFoundFromParent, ErrorNotFoundInChild, ExposeVisitor,
    OfferVisitor, RouteBundle, Sources,
};
use crate::mapper::NoopRouteMapper;
use crate::NoopVisitor;
use cm_rust::{ExposeDecl, NameMapping, OfferDecl, OfferServiceDecl};
use cm_types::Name;
use derivative::Derivative;
use futures::future::BoxFuture;
use moniker::{ChildName, ExtendedMoniker};
use std::collections::HashSet;
use std::sync::Arc;

/// Provides capabilities exposed by an anonymized aggregates.
///
/// Given a set of collections and static children and the name of a capability, this provider
/// returns a list of children within them that expose the capability, and routes to a particular
/// child's exposed capability with that name.
///
/// This is used during collection routing from anonymized aggregate service instances.
#[derive(Clone)]
pub struct AnonymizedAggregateServiceProvider<C: ComponentInstanceInterface> {
    /// Component that defines the aggregate.
    pub containing_component: WeakComponentInstanceInterface<C>,

    /// The members relative to `containing_component` that make up the aggregate.
    pub members: Vec<AggregateMember>,

    /// Name of the capability as exposed by children in the collection.
    pub capability_name: Name,

    pub capability_type: cm_rust::CapabilityTypeName,

    pub sources: Sources,
}

impl<C> AnonymizedAggregateServiceProvider<C>
where
    C: ComponentInstanceInterface + 'static,
{
    /// Creates a new `AnonymizedAggregateServiceProvider` from a
    /// `CapabilitySource::AnonymizedAggregate`. The `source_component_instance` must have a
    /// moniker that matches the moniker value in the source. None is returned if source is not a
    /// `CapabilitySource::AnonymizedAggregate`.
    pub async fn new_from_capability_source(
        source: &CapabilitySource,
        source_component_instance: &Arc<C>,
    ) -> Option<Self> {
        let CapabilitySource::AnonymizedAggregate(AnonymizedAggregateSource {
            capability,
            moniker,
            members,
            sources,
        }) = source
        else {
            return None;
        };
        assert_eq!(&moniker, &source_component_instance.moniker(), "the provided source_component_instance does not match the moniker in the capability source");
        Some(Self {
            containing_component: source_component_instance.as_weak(),
            members: members.clone(),
            capability_name: capability.source_name().clone(),
            capability_type: capability.type_name(),
            sources: sources.clone(),
        })
    }

    /// Returns a list of instances contributing capabilities to this provider.
    ///
    /// In the case of service capabilities, they are *not* instances inside that service, but
    /// rather service capabilities with the same name that are exposed by different components.
    pub async fn list_instances(&self) -> Result<Vec<AggregateInstance>, RoutingError> {
        let mut instances = Vec::new();
        let component = self.containing_component.upgrade()?;
        let mut child_components = vec![];
        let mut parent = None;
        let mut include_self = false;
        {
            let resolved_state = component.lock_resolved_state().await?;
            for s in &self.members {
                match s {
                    AggregateMember::Child(child_ref) => {
                        let child_name = child_ref.clone().into();
                        if let Some(child) = resolved_state.get_child(&child_name) {
                            child_components.push((child_name.clone(), child.clone()));
                        }
                    }
                    AggregateMember::Collection(collection) => {
                        child_components.extend(resolved_state.children_in_collection(&collection));
                    }
                    AggregateMember::Parent => {
                        if let Ok(p) = component.try_get_parent() {
                            match p {
                                ExtendedInstanceInterface::AboveRoot(_) => {
                                    // Services from above parent are not supported.
                                }
                                ExtendedInstanceInterface::Component(p) => {
                                    parent = Some(p);
                                }
                            }
                        }
                    }
                    AggregateMember::Self_ => {
                        include_self = true;
                    }
                }
            }
        }
        for (child_name, child_component) in child_components {
            let child_exposes = child_component.lock_resolved_state().await.map(|c| c.exposes());
            match child_exposes {
                Ok(child_exposes) => {
                    if let Some(_) = legacy_router::find_matching_exposes(
                        self.capability_type,
                        &self.capability_name,
                        &child_exposes,
                    ) {
                        instances.push(AggregateInstance::Child(child_name.clone()));
                    }
                }
                // Ignore errors. One misbehaving component should not affect the entire collection.
                Err(_) => {}
            }
        }
        if let Some(parent) = parent {
            let parent_offers = parent.lock_resolved_state().await.map(|c| c.offers());
            match parent_offers {
                Ok(parent_offers) => {
                    let child_moniker = component.child_moniker().expect("ChildName should exist");
                    if let Some(_) = legacy_router::find_matching_offers(
                        self.capability_type,
                        &self.capability_name,
                        &child_moniker,
                        &parent_offers,
                    ) {
                        instances.push(AggregateInstance::Parent);
                    }
                }
                // Ignore errors. One misbehaving component should not affect the entire collection.
                Err(_) => {}
            }
        }
        if include_self {
            instances.push(AggregateInstance::Self_);
        }
        Ok(instances)
    }

    /// Returns a `CapabilitySource` to a capability exposed by a child.
    ///
    /// `instance` is the name of the child that exposes the capability, as returned by
    /// `list_instances`.
    pub async fn route_instance<V>(
        &self,
        instance: &AggregateInstance,
        visitor: &mut V,
    ) -> Result<CapabilitySource, RoutingError>
    where
        V: OfferVisitor,
        V: ExposeVisitor,
        V: CapabilityVisitor,
        V: Clone + Send + Sync + 'static,
    {
        match instance {
            AggregateInstance::Child(name) => self.route_child_instance(&name, visitor).await,
            AggregateInstance::Parent => self.route_parent_instance(visitor).await,
            AggregateInstance::Self_ => self.route_self_instance(visitor).await,
        }
    }

    async fn route_child_instance<V>(
        &self,
        instance: &ChildName,
        visitor: &mut V,
    ) -> Result<CapabilitySource, RoutingError>
    where
        V: OfferVisitor,
        V: ExposeVisitor,
        V: CapabilityVisitor,
        V: Clone + Send + Sync + 'static,
    {
        let containing_component = self.containing_component.upgrade()?;
        let child_component =
            containing_component.lock_resolved_state().await?.get_child(instance).ok_or_else(
                || RoutingError::OfferFromChildInstanceNotFound {
                    child_moniker: instance.clone(),
                    moniker: containing_component.moniker().clone(),
                    capability_id: self.capability_name.clone().into(),
                },
            )?;

        let child_exposes = child_component.lock_resolved_state().await?.exposes();
        let child_exposes = legacy_router::find_matching_exposes(
            self.capability_type,
            &self.capability_name,
            &child_exposes,
        )
        .ok_or_else(|| {
            ExposeDecl::error_not_found_in_child(
                containing_component.moniker().clone(),
                instance.clone(),
                self.capability_name.clone(),
            )
        })?;
        legacy_router::route_from_expose(
            child_exposes,
            child_component,
            self.sources.clone(),
            visitor,
            &mut NoopRouteMapper,
        )
        .await
    }

    async fn route_parent_instance<V>(
        &self,
        visitor: &mut V,
    ) -> Result<CapabilitySource, RoutingError>
    where
        V: OfferVisitor,
        V: ExposeVisitor,
        V: CapabilityVisitor,
        V: Clone + Send + Sync + 'static,
    {
        let containing_component = self.containing_component.upgrade()?;
        let parent = match containing_component.try_get_parent().map_err(|_| {
            RoutingError::OfferFromParentNotFound {
                moniker: containing_component.moniker().clone(),
                capability_id: self.capability_name.clone().into(),
            }
        })? {
            ExtendedInstanceInterface::AboveRoot(_) => {
                return Err(RoutingError::unsupported_route_source(
                    ExtendedMoniker::ComponentManager,
                    "service above parent",
                ));
            }
            ExtendedInstanceInterface::Component(p) => p,
        };

        let child_moniker = containing_component.child_moniker().expect("ChildName should exist");
        let parent_offers = parent.lock_resolved_state().await?.offers();
        let parent_offers = legacy_router::find_matching_offers(
            self.capability_type,
            &self.capability_name,
            &child_moniker,
            &parent_offers,
        )
        .ok_or_else(|| {
            OfferDecl::error_not_found_from_parent(
                containing_component.moniker().clone(),
                self.capability_name.clone(),
            )
        })?;
        legacy_router::route_from_offer(
            parent_offers,
            parent,
            self.sources.clone(),
            visitor,
            &mut NoopRouteMapper,
        )
        .await
    }

    async fn route_self_instance<V>(
        &self,
        visitor: &mut V,
    ) -> Result<CapabilitySource, RoutingError>
    where
        V: OfferVisitor,
        V: ExposeVisitor,
        V: CapabilityVisitor,
        V: Clone + Send + Sync + 'static,
    {
        let containing_component = self.containing_component.upgrade()?;
        legacy_router::route_from_self_by_name(
            &self.capability_name,
            containing_component,
            self.sources.clone(),
            visitor,
            &mut NoopRouteMapper,
        )
        .await
    }
}

pub fn new_filtered_aggregate_from_capability_source<C>(
    source: CapabilitySource,
    aggregation_component: WeakComponentInstanceInterface<C>,
) -> Box<dyn FilteredAggregateCapabilityProvider>
where
    C: ComponentInstanceInterface + 'static,
{
    match source {
        CapabilitySource::FilteredProvider(FilteredProviderSource {
            capability: _,
            moniker,
            service_capability,
            offer_service_decl,
        }) => {
            assert_eq!(&moniker, &aggregation_component.moniker);
            Box::new(OfferFilteredServiceProvider::new(
                offer_service_decl,
                aggregation_component,
                service_capability,
            ))
        }
        CapabilitySource::FilteredAggregateProvider(FilteredAggregateProviderSource {
            capability: _,
            moniker,
            offer_service_decls,
            sources,
        }) => {
            assert_eq!(&moniker, &aggregation_component.moniker);
            Box::new(OfferAggregateServiceProvider::new(
                offer_service_decls,
                aggregation_component,
                sources,
                NoopVisitor {},
            ))
        }
        other_source => panic!("unexpected capability source: {}", other_source),
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub struct OfferFilteredServiceProvider<C: ComponentInstanceInterface> {
    /// Component that offered the filtered service
    component: WeakComponentInstanceInterface<C>,
    /// The service capability
    capability: ComponentCapability,
    /// The service offer that has a filter.
    offer_decl: OfferServiceDecl,
}

impl<C> OfferFilteredServiceProvider<C>
where
    C: ComponentInstanceInterface + 'static,
{
    pub fn new(
        offer_decl: OfferServiceDecl,
        component: WeakComponentInstanceInterface<C>,
        capability: ComponentCapability,
    ) -> Self {
        Self { offer_decl, component, capability }
    }
}

impl<C> FilteredAggregateCapabilityProvider for OfferFilteredServiceProvider<C>
where
    C: ComponentInstanceInterface + 'static,
{
    fn route_instances(
        &self,
    ) -> Vec<BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData, RoutingError>>> {
        let capability_source = CapabilitySource::Component(ComponentSource {
            capability: self.capability.clone(),
            moniker: self.component.moniker.clone(),
        });
        let instance_filter = get_instance_filter(&self.offer_decl);
        let fut = async move {
            Ok(FilteredAggregateCapabilityRouteData { capability_source, instance_filter })
        };
        // Without the explicit type, this does not compile
        let out: Vec<BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData, RoutingError>>> =
            vec![Box::pin(fut)];
        out
    }

    fn clone_boxed(&self) -> Box<dyn FilteredAggregateCapabilityProvider> {
        Box::new(self.clone())
    }
}

fn get_instance_filter(offer_decl: &OfferServiceDecl) -> Vec<NameMapping> {
    let renamed_instances = offer_decl.renamed_instances.as_ref().unwrap_or_else(|| {
        static EMPTY_VEC: Vec<NameMapping> = vec![];
        &EMPTY_VEC
    });
    if !renamed_instances.is_empty() {
        let source_instance_filter: HashSet<_> = offer_decl
            .source_instance_filter
            .as_ref()
            .unwrap_or_else(|| {
                static EMPTY_VEC: Vec<Name> = vec![];
                &EMPTY_VEC
            })
            .iter()
            .map(|s| s.as_str())
            .collect();
        renamed_instances
            .clone()
            .into_iter()
            .filter_map(|m| {
                if source_instance_filter.is_empty()
                    || source_instance_filter.contains(&m.target_name.as_str())
                {
                    Some(NameMapping { source_name: m.source_name, target_name: m.target_name })
                } else {
                    None
                }
            })
            .collect()
    } else {
        offer_decl
            .source_instance_filter
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|n| NameMapping { source_name: n.clone(), target_name: n })
            .collect()
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = "V: Clone"))]
pub struct OfferAggregateServiceProvider<C: ComponentInstanceInterface, V> {
    /// Component that offered the aggregate service
    component: WeakComponentInstanceInterface<C>,

    /// List of offer decl to follow for routing each service provider used in the overall aggregation
    offer_decls: Vec<OfferServiceDecl>,

    sources: Sources,
    visitor: V,
}

impl<C, V> OfferAggregateServiceProvider<C, V>
where
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor + ExposeVisitor + CapabilityVisitor,
    V: Send + Sync + Clone + 'static,
{
    pub fn new(
        offer_decls: Vec<OfferServiceDecl>,
        component: WeakComponentInstanceInterface<C>,
        sources: Sources,
        visitor: V,
    ) -> Self {
        Self { offer_decls, sources, visitor, component }
    }
}

impl<C, V> FilteredAggregateCapabilityProvider for OfferAggregateServiceProvider<C, V>
where
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor + ExposeVisitor + CapabilityVisitor,
    V: Send + Sync + Clone + 'static,
{
    fn route_instances(
        &self,
    ) -> Vec<BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData, RoutingError>>> {
        // Without the explicit type, this does not compile
        let mut out: Vec<
            BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData, RoutingError>>,
        > = vec![];
        for offer_decl in &self.offer_decls {
            let instance_filter = get_instance_filter(offer_decl);
            if instance_filter.is_empty() {
                continue;
            }
            let mut offer_decl = offer_decl.clone();
            offer_decl.source_instance_filter = None;
            offer_decl.renamed_instances = None;
            let offer_decl = OfferDecl::Service(offer_decl);
            let fut = async {
                let component = self.component.upgrade().map_err(|e| {
                    RoutingError::unsupported_route_source(
                        self.component.moniker.clone(),
                        format!("error upgrading aggregation point component {}", e),
                    )
                })?;
                let capability_source = legacy_router::route_from_offer(
                    RouteBundle::from_offer(offer_decl),
                    component,
                    self.sources.clone(),
                    &mut self.visitor.clone(),
                    &mut NoopRouteMapper,
                )
                .await?;
                Ok(FilteredAggregateCapabilityRouteData { capability_source, instance_filter })
            };
            out.push(Box::pin(fut));
        }
        out
    }

    fn clone_boxed(&self) -> Box<dyn FilteredAggregateCapabilityProvider> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_rust_testing::*;

    #[test]
    fn test_get_instance_filter() {
        fn get_instance_filter(
            source_instance_filter: Option<Vec<Name>>,
            renamed_instances: Option<Vec<NameMapping>>,
        ) -> Vec<NameMapping> {
            let offer = OfferBuilder::service()
                .name("foo")
                .target_name("bar")
                .source(cm_rust::OfferSource::Parent)
                .target_static_child("a")
                .build();
            let OfferDecl::Service(mut offer) = offer else {
                unreachable!();
            };
            offer.source_instance_filter = source_instance_filter;
            offer.renamed_instances = renamed_instances;
            super::get_instance_filter(&offer)
        }

        assert_eq!(get_instance_filter(None, None), vec![]);
        assert_eq!(get_instance_filter(Some(vec![]), Some(vec![])), vec![]);
        let same_name_map = vec![
            NameMapping { source_name: "a".parse().unwrap(), target_name: "a".parse().unwrap() },
            NameMapping { source_name: "b".parse().unwrap(), target_name: "b".parse().unwrap() },
        ];
        assert_eq!(
            get_instance_filter(Some(vec!["a".parse().unwrap(), "b".parse().unwrap()]), None),
            same_name_map
        );
        assert_eq!(
            get_instance_filter(
                Some(vec!["a".parse().unwrap(), "b".parse().unwrap()]),
                Some(vec![])
            ),
            same_name_map
        );
        let renamed_map = vec![
            NameMapping { source_name: "one".parse().unwrap(), target_name: "a".parse().unwrap() },
            NameMapping { source_name: "two".parse().unwrap(), target_name: "b".parse().unwrap() },
        ];
        assert_eq!(get_instance_filter(None, Some(renamed_map.clone())), renamed_map);
        assert_eq!(get_instance_filter(Some(vec![]), Some(renamed_map.clone())), renamed_map);
        assert_eq!(
            get_instance_filter(Some(vec!["b".parse().unwrap()]), Some(renamed_map.clone())),
            vec![NameMapping {
                source_name: "two".parse().unwrap(),
                target_name: "b".parse().unwrap()
            }]
        );
    }
}
