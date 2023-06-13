// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability_source::{
            CapabilitySource, CollectionAggregateCapabilityProvider,
            OfferAggregateCapabilityProvider,
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
    cm_rust::{ExposeDecl, ExposeDeclCommon, OfferDecl, OfferDeclCommon, OfferServiceDecl},
    cm_types::Name,
    derivative::Derivative,
    from_enum::FromEnum,
    futures::future::BoxFuture,
    moniker::{ChildMoniker, ChildMonikerBase},
    std::sync::Arc,
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
    async fn list_instances(&self) -> Result<Vec<ChildMoniker>, RoutingError> {
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
        instance: &ChildMoniker,
    ) -> Result<CapabilitySource<C>, RoutingError> {
        if instance.collection().is_none()
            || !self.collections.contains(instance.collection().unwrap())
        {
            return Err(RoutingError::UnexpectedChildInAggregate {
                child_moniker: instance.clone(),
                moniker: self.collection_component.abs_moniker.clone(),
                capability: self.capability_name.clone(),
            });
        }

        let collection_component = self.collection_component.upgrade()?;
        let (child_moniker, child_component): (ChildMoniker, Arc<C>) = {
            collection_component
                .lock_resolved_state()
                .await?
                .children_in_collection(instance.collection().unwrap())
                .into_iter()
                .find(|child| child.0.name() == instance.name())
                .ok_or_else(|| RoutingError::OfferFromChildInstanceNotFound {
                    child_moniker: instance.clone(),
                    moniker: collection_component.abs_moniker().clone(),
                    capability_id: self.capability_name.clone().into(),
                })?
        };

        let child_exposes = child_component.lock_resolved_state().await?.exposes();
        let child_exposes = router::find_matching_exposes(&self.capability_name, &child_exposes)
            .ok_or_else(|| {
                E::error_not_found_in_child(
                    collection_component.abs_moniker().clone(),
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
        offer_service_decls: Vec<OfferServiceDecl>,
        component: WeakComponentInstanceInterface<C>,
        sources: S,
        visitor: V,
        mapper: M,
    ) -> Self {
        let single_instance_decls: Vec<OfferServiceDecl> = offer_service_decls
            .iter()
            .map(|o| {
                o.source_instance_filter.iter().flatten().map(move |instance_name| {
                    // Create a service decl for each filtered service instance so that when the aggregate
                    // capability routes an instance each entry can ignore the component name.
                    let mut single_instance_decl = o.clone();
                    single_instance_decl.source_instance_filter = Some(vec![instance_name.clone()]);
                    single_instance_decl
                })
            })
            .flatten()
            .collect();
        Self {
            offer_decls: single_instance_decls,
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
    ) -> Vec<BoxFuture<'_, Result<(CapabilitySource<C>, Vec<String>), RoutingError>>> {
        // Without the explicit type, this does not compile
        let mut out: Vec<BoxFuture<'_, Result<(CapabilitySource<C>, Vec<String>), RoutingError>>> =
            vec![];
        for offer_decl in &self.offer_decls {
            let filter =
                offer_decl.source_instance_filter.as_ref().map(Clone::clone).unwrap_or_default();
            if filter.is_empty() {
                continue;
            }
            // The visitor which we inherited from the router is parameterized on the generic
            // type `O`. This construction with from_enum is a roundabout way of
            // converting from the concrete variant type to the generic.
            let offer_decl = OfferDecl::Service(offer_decl.clone());
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
                Ok((capability_source, filter))
            };
            out.push(Box::pin(fut));
        }
        out
    }

    fn clone_boxed(&self) -> Box<dyn OfferAggregateCapabilityProvider<C>> {
        Box::new(self.clone())
    }
}
