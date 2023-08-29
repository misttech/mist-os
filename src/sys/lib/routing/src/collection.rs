// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability_source::{
            CapabilitySource, CollectionAggregateCapabilityProvider, ComponentCapability,
            OfferAggregateCapabilityProvider, OfferAggregateCapabilityRouteData,
        },
        component_instance::{
            ComponentInstanceInterface, ResolvedInstanceInterface, WeakComponentInstanceInterface,
        },
        error::RoutingError,
        mapper::DebugRouteMapper,
        router::{
            self, CapabilityVisitor, ErrorNotFoundFromParent, ErrorNotFoundInChild, ExposeVisitor,
            OfferVisitor, RouteBundle, Sources,
        },
        Never, RouteInfo,
    },
    async_trait::async_trait,
    cm_rust::{
        ExposeDecl, ExposeDeclCommon, NameMapping, OfferDecl, OfferDeclCommon, OfferServiceDecl,
    },
    cm_types::Name,
    derivative::Derivative,
    from_enum::FromEnum,
    futures::future::BoxFuture,
    moniker::{ChildName, ChildNameBase},
    std::{collections::HashSet, sync::Arc},
};

/// Provides capabilities exposed by children in a collection.
///
/// Given a collection and the name of a capability, this provider returns a list of children
/// within the collection that expose the capability, and routes to a particular child's exposed
/// capability with that name.
///
/// This is used during collection routing to aggregate service instances across
/// all children within the collection.
#[derive(Derivative)]
#[derivative(Clone(bound = "E: Clone, S: Clone, V: Clone, M: Clone"))]
pub(super) struct CollectionAggregateServiceProvider<C: ComponentInstanceInterface, E, S, V, M> {
    /// Component that contains the collection.
    pub collection_component: WeakComponentInstanceInterface<C>,

    pub phantom_expose: std::marker::PhantomData<E>,

    /// Names of the collections within `collection_component` that are being aggregated.
    pub collections: Vec<Name>,

    /// Name of the capability as exposed by children in the collection.
    pub capability_name: Name,

    pub sources: S,
    pub visitor: V,
    pub mapper: M,
}

#[async_trait]
impl<C, E, S, V, M> CollectionAggregateCapabilityProvider<C>
    for CollectionAggregateServiceProvider<C, E, S, V, M>
where
    C: ComponentInstanceInterface + 'static,
    E: ExposeDeclCommon
        + FromEnum<cm_rust::ExposeDecl>
        + ErrorNotFoundInChild
        + Into<ExposeDecl>
        + Clone
        + 'static,
    S: Sources + 'static,
    V: ExposeVisitor<ExposeDecl = E>,
    V: CapabilityVisitor<CapabilityDecl = S::CapabilityDecl>,
    V: Clone + Send + Sync + 'static,
    M: DebugRouteMapper + Send + Sync + Clone + 'static,
{
    /// Returns a list of instances of capabilities in this provider.
    ///
    /// Instances correspond to the names of children in the collection that expose the capability
    /// with the name `capability_name`.
    ///
    /// In the case of service capabilities, they are *not* instances inside that service, but
    /// rather service capabilities with the same name that are exposed by different children.
    async fn list_instances(&self) -> Result<Vec<ChildName>, RoutingError> {
        let mut instances = Vec::new();
        let component = self.collection_component.upgrade()?;
        let mut child_components = vec![];
        for collection in &self.collections {
            child_components
                .extend(component.lock_resolved_state().await?.children_in_collection(collection));
        }
        for (moniker, child_component) in child_components {
            let child_exposes = child_component.lock_resolved_state().await.map(|c| c.exposes());
            match child_exposes {
                Ok(child_exposes) => {
                    if let Some(_) =
                        router::find_matching_exposes::<E>(&self.capability_name, &child_exposes)
                    {
                        instances.push(moniker.clone());
                    }
                }
                // Ignore errors. One misbehaving component should not affect the entire collection.
                Err(_) => {}
            }
        }
        Ok(instances)
    }

    /// Returns a `CapabilitySource` to a capability exposed by a child.
    ///
    /// `instance` is the name of the child that exposes the capability, as returned by
    /// `list_instances`.
    async fn route_instance(
        &self,
        instance: &ChildName,
    ) -> Result<CapabilitySource<C>, RoutingError> {
        if instance.collection().is_none()
            || !self.collections.contains(instance.collection().unwrap())
        {
            return Err(RoutingError::UnexpectedChildInAggregate {
                child_moniker: instance.clone(),
                moniker: self.collection_component.moniker.clone(),
                capability: self.capability_name.clone(),
            });
        }

        let collection_component = self.collection_component.upgrade()?;
        let (child_moniker, child_component): (ChildName, Arc<C>) = {
            collection_component
                .lock_resolved_state()
                .await?
                .children_in_collection(instance.collection().unwrap())
                .into_iter()
                .find(|child| child.0.name() == instance.name())
                .ok_or_else(|| RoutingError::OfferFromChildInstanceNotFound {
                    child_moniker: instance.clone(),
                    moniker: collection_component.moniker().clone(),
                    capability_id: self.capability_name.clone().into(),
                })?
        };

        let child_exposes = child_component.lock_resolved_state().await?.exposes();
        let child_exposes = router::find_matching_exposes(&self.capability_name, &child_exposes)
            .ok_or_else(|| {
                E::error_not_found_in_child(
                    collection_component.moniker().clone(),
                    child_moniker,
                    self.capability_name.clone(),
                )
            })?;
        router::route_from_expose(
            child_exposes,
            child_component,
            self.sources.clone(),
            &mut self.visitor.clone(),
            &mut self.mapper.clone(),
            &mut vec![] as &mut Vec<RouteInfo<_, Never, _>>,
        )
        .await
    }

    fn clone_boxed(&self) -> Box<dyn CollectionAggregateCapabilityProvider<C>> {
        Box::new(self.clone())
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub(super) struct OfferFilteredServiceProvider<C: ComponentInstanceInterface> {
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
    pub(super) fn new(
        offer_decl: OfferServiceDecl,
        component: WeakComponentInstanceInterface<C>,
        capability: ComponentCapability,
    ) -> Self {
        Self { offer_decl, component, capability }
    }
}

impl<C> OfferAggregateCapabilityProvider<C> for OfferFilteredServiceProvider<C>
where
    C: ComponentInstanceInterface + 'static,
{
    fn route_instances(
        &self,
    ) -> Vec<BoxFuture<'_, Result<OfferAggregateCapabilityRouteData<C>, RoutingError>>> {
        let capability_source = CapabilitySource::Component {
            capability: self.capability.clone(),
            component: self.component.clone(),
        };
        let instance_filter = get_instance_filter(&self.offer_decl);
        let fut = async move {
            Ok(OfferAggregateCapabilityRouteData { capability_source, instance_filter })
        };
        // Without the explicit type, this does not compile
        let mut out: Vec<
            BoxFuture<'_, Result<OfferAggregateCapabilityRouteData<C>, RoutingError>>,
        > = vec![];
        out.push(Box::pin(fut));
        out
    }

    fn clone_boxed(&self) -> Box<dyn OfferAggregateCapabilityProvider<C>> {
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
                static EMPTY_VEC: Vec<String> = vec![];
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
#[derivative(Clone(bound = "S: Clone, M: Clone, V: Clone"))]
pub(super) struct OfferAggregateServiceProvider<C: ComponentInstanceInterface, O, E, S, M, V> {
    /// Component that offered the aggregate service
    component: WeakComponentInstanceInterface<C>,

    phantom_offer: std::marker::PhantomData<O>,
    phantom_expose: std::marker::PhantomData<E>,

    /// List of offer decl to follow for routing each service provider used in the overall aggregation
    offer_decls: Vec<OfferServiceDecl>,

    sources: S,
    visitor: V,
    mapper: M,
}

impl<C, O, E, S, M, V> OfferAggregateServiceProvider<C, O, E, S, M, V>
where
    C: ComponentInstanceInterface + 'static,
    E: ExposeDeclCommon
        + ErrorNotFoundInChild
        + FromEnum<ExposeDecl>
        + Into<ExposeDecl>
        + Clone
        + 'static,
    O: OfferDeclCommon
        + ErrorNotFoundFromParent
        + ErrorNotFoundInChild
        + FromEnum<OfferDecl>
        + Into<OfferDecl>
        + Clone
        + 'static,
    S: Sources + 'static,
    V: OfferVisitor<OfferDecl = O>
        + ExposeVisitor<ExposeDecl = E>
        + CapabilityVisitor<CapabilityDecl = S::CapabilityDecl>,
    V: Send + Sync + Clone + 'static,
    M: DebugRouteMapper + Send + Sync + Clone + 'static,
{
    pub(super) fn new(
        offer_decls: Vec<OfferServiceDecl>,
        component: WeakComponentInstanceInterface<C>,
        sources: S,
        visitor: V,
        mapper: M,
    ) -> Self {
        Self {
            offer_decls,
            phantom_expose: std::marker::PhantomData::<E> {},
            phantom_offer: std::marker::PhantomData::<O> {},
            sources,
            visitor,
            component,
            mapper,
        }
    }
}

impl<C, O, E, S, M, V> OfferAggregateCapabilityProvider<C>
    for OfferAggregateServiceProvider<C, O, E, S, M, V>
where
    C: ComponentInstanceInterface + 'static,
    E: ExposeDeclCommon
        + ErrorNotFoundInChild
        + FromEnum<ExposeDecl>
        + Into<ExposeDecl>
        + Clone
        + 'static,
    O: OfferDeclCommon
        + ErrorNotFoundFromParent
        + ErrorNotFoundInChild
        + FromEnum<OfferDecl>
        + Into<OfferDecl>
        + Clone
        + 'static,
    S: Sources + 'static,
    V: OfferVisitor<OfferDecl = O>
        + ExposeVisitor<ExposeDecl = E>
        + CapabilityVisitor<CapabilityDecl = S::CapabilityDecl>,
    V: Send + Sync + Clone + 'static,
    M: DebugRouteMapper + Send + Sync + Clone + 'static,
{
    fn route_instances(
        &self,
    ) -> Vec<BoxFuture<'_, Result<OfferAggregateCapabilityRouteData<C>, RoutingError>>> {
        // Without the explicit type, this does not compile
        let mut out: Vec<
            BoxFuture<'_, Result<OfferAggregateCapabilityRouteData<C>, RoutingError>>,
        > = vec![];
        for offer_decl in &self.offer_decls {
            let instance_filter = get_instance_filter(offer_decl);
            if instance_filter.is_empty() {
                continue;
            }
            // The visitor which we inherited from the router is parameterized on the generic
            // type `O`. This construction with from_enum is a roundabout way of
            // converting from the concrete variant type to the generic.
            let mut offer_decl = offer_decl.clone();
            offer_decl.source_instance_filter = None;
            offer_decl.renamed_instances = None;
            let offer_decl = OfferDecl::Service(offer_decl);
            let offer_decl = O::from_enum(&offer_decl).unwrap().clone();
            let fut = async {
                let component = self.component.upgrade().map_err(|e| {
                    RoutingError::unsupported_route_source(format!(
                        "error upgrading aggregation point component {}",
                        e
                    ))
                })?;
                let capability_source = router::route_from_offer(
                    RouteBundle::from_offer(offer_decl),
                    component,
                    self.sources.clone(),
                    &mut self.visitor.clone(),
                    &mut self.mapper.clone(),
                    &mut vec![],
                )
                .await?;
                Ok(OfferAggregateCapabilityRouteData { capability_source, instance_filter })
            };
            out.push(Box::pin(fut));
        }
        out
    }

    fn clone_boxed(&self) -> Box<dyn OfferAggregateCapabilityProvider<C>> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_rust::{Availability, ChildRef, OfferSource, OfferTarget};

    #[test]
    fn test_get_instance_filter() {
        fn get_instance_filter(
            source_instance_filter: Option<Vec<String>>,
            renamed_instances: Option<Vec<NameMapping>>,
        ) -> Vec<NameMapping> {
            super::get_instance_filter(&OfferServiceDecl {
                source: OfferSource::Parent,
                source_name: "foo".parse().unwrap(),
                target: OfferTarget::Child(ChildRef { name: "a".into(), collection: None }),
                target_name: "bar".parse().unwrap(),
                source_instance_filter,
                renamed_instances,
                availability: Availability::Required,
            })
        }

        assert_eq!(get_instance_filter(None, None), vec![]);
        assert_eq!(get_instance_filter(Some(vec![]), Some(vec![])), vec![]);
        let same_name_map = vec![
            NameMapping { source_name: "a".into(), target_name: "a".into() },
            NameMapping { source_name: "b".into(), target_name: "b".into() },
        ];
        assert_eq!(get_instance_filter(Some(vec!["a".into(), "b".into()]), None), same_name_map);
        assert_eq!(
            get_instance_filter(Some(vec!["a".into(), "b".into()]), Some(vec![])),
            same_name_map
        );
        let renamed_map = vec![
            NameMapping { source_name: "one".into(), target_name: "a".into() },
            NameMapping { source_name: "two".into(), target_name: "b".into() },
        ];
        assert_eq!(get_instance_filter(None, Some(renamed_map.clone())), renamed_map);
        assert_eq!(get_instance_filter(Some(vec![]), Some(renamed_map.clone())), renamed_map);
        assert_eq!(
            get_instance_filter(Some(vec!["b".into()]), Some(renamed_map.clone())),
            vec![NameMapping { source_name: "two".into(), target_name: "b".into() }]
        );
    }
}
