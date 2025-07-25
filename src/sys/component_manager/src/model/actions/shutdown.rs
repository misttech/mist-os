// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![allow(unused_imports)]
#![allow(dead_code)]

use crate::model::actions::{Action, ActionKey, ActionsManager};
use crate::model::component::instance::{InstanceState, ResolvedInstanceState};
use crate::model::component::ComponentInstance;
use async_trait::async_trait;
use cm_rust::{
    CapabilityDecl, ChildRef, CollectionDecl, DependencyType, DictionaryDecl, EnvironmentDecl,
    ExposeDecl, NativeIntoFidl, OfferConfigurationDecl, OfferDecl, OfferDictionaryDecl,
    OfferDirectoryDecl, OfferProtocolDecl, OfferResolverDecl, OfferRunnerDecl, OfferServiceDecl,
    OfferSource, OfferStorageDecl, OfferTarget, RegistrationDeclCommon, RegistrationSource,
    SourcePath, StorageDecl, StorageDirectorySource, UseConfigurationDecl, UseDecl,
    UseDirectoryDecl, UseEventStreamDecl, UseProtocolDecl, UseRunnerDecl, UseServiceDecl,
    UseSource, UseStorageDecl,
};
use cm_types::{IterablePath, Name};
use errors::{ActionError, ShutdownActionError};
use futures::future::select_all;
use futures::prelude::*;
use log::*;
use moniker::ChildName;
use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::{fmt, iter};

use cm_graph::DependencyNode;
use directed_graph::DirectedGraph;
use {fidl_fuchsia_component_decl as fdecl, fuchsia_async as fasync};

/// Shuts down all component instances in this component (stops them and guarantees they will never
/// be started again).
pub struct ShutdownAction {
    shutdown_type: ShutdownType,
}

/// Indicates the type of shutdown being performed.
#[derive(Clone, Copy, PartialEq)]
pub enum ShutdownType {
    /// An individual component instance was shut down. For example, this is used when
    /// a component instance is destroyed.
    Instance,

    /// The entire system under this component_manager was shutdown on behalf of
    /// a call to SystemController/Shutdown.
    System,
}

impl ShutdownAction {
    pub fn new(shutdown_type: ShutdownType) -> Self {
        Self { shutdown_type }
    }
}

#[async_trait]
impl Action for ShutdownAction {
    async fn handle(self, component: Arc<ComponentInstance>) -> Result<(), ActionError> {
        do_shutdown(&component, self.shutdown_type).await
    }
    fn key(&self) -> ActionKey {
        ActionKey::Shutdown
    }
}

/// Used to track information during the shutdown process.
struct ShutdownComponentInfo<'a> {
    dependency_node: DependencyNode<'a>,
    component: Arc<ComponentInstance>,
}

async fn shutdown_component<'a>(
    target: ShutdownComponentInfo<'a>,
    shutdown_type: ShutdownType,
) -> Result<DependencyNode<'a>, ActionError> {
    match target.dependency_node {
        DependencyNode::Self_ => {
            // TODO: Put `self` in a "shutting down" state so that if it creates
            // new instances after this point, they are created in a shut down
            // state.
            //
            // NOTE: we cannot register a `StopAction { shutdown: true }` action because
            // that would be overridden by any concurrent `StopAction { shutdown: false }`.
            // More over, for reasons detailed in
            // https://fxrev.dev/I8ccfa1deed368f2ccb77cde0d713f3af221f7450, an in-progress
            // Shutdown action will block Stop actions, so registering the latter will deadlock.
            target.component.stop_instance_internal(true).await?;
        }
        DependencyNode::Child(..) => {
            ActionsManager::register(target.component, ShutdownAction::new(shutdown_type)).await?;
        }
        _ => {
            // This is just an intermediate node that exists to track dependencies on storage
            // and dictionary capabilities from Self, which aren't associated with the running
            // program. Nothing to do.
        }
    }

    Ok(target.dependency_node.clone())
}

/// Contains all the information necessary to shut down a single component
/// instance. The shutdown process is recursive: shutting down a component requires
/// shutting down all of its children first.
struct ShutdownJob {
    /// The type of shutdown being performed. For debug purposes.
    shutdown_type: ShutdownType,
    /// The declaration of the component being shut down.
    component_decl: fdecl::Component,
    /// A reference-counted pointer to the component instance being shut down.
    instance: Arc<ComponentInstance>,
    /// A map of the live children of the `instance`.
    children: HashMap<ChildName, Arc<ComponentInstance>>,
    /// Dynamic offers from a parent to a collection that this instance may be a part of.
    dynamic_offers: Vec<fdecl::Offer>,
}

/// ShutdownJob encapsulates the logic and state require to shutdown a component.
impl ShutdownJob {
    /// Creates a new ShutdownJob by examining the Component's declaration and
    /// runtime state to build up the necessary data structures to stop
    /// components in the component in dependency order.
    pub async fn new(
        instance: &Arc<ComponentInstance>,
        state: &ResolvedInstanceState,
        shutdown_type: ShutdownType,
    ) -> ShutdownJob {
        let component_decl: fdecl::Component = state.decl().to_owned().into();

        let dynamic_offers: Vec<fdecl::Offer> =
            state.dynamic_offers.iter().map(|g| g.clone().native_into_fidl()).collect();

        let children = state.children.clone();

        let new_job = ShutdownJob {
            shutdown_type,
            component_decl,
            instance: instance.clone(),
            children,
            dynamic_offers,
        };
        return new_job;
    }

    /// Perform shutdown of the Component that was used to create this ShutdownJob.
    pub async fn execute(&mut self) -> Result<(), ActionError> {
        let dynamic_offers = self.dynamic_offers.clone();

        let mut dynamic_children = vec![];
        dynamic_children.extend(
            self.children.keys().filter_map(|key| {
                key.collection().map(|coll| (key.name().as_str(), coll.as_str()))
            }),
        );

        let deps = process_deps(&self.component_decl, &dynamic_children, &dynamic_offers);

        let sorted_map = deps.topological_sort().map_err(|_| ActionError::ShutdownError {
            err: ShutdownActionError::CyclesDetected {},
        })?;

        for dependency_node in sorted_map.into_iter() {
            let component = match dependency_node {
                DependencyNode::Child(name, coll) => {
                    let moniker = &ChildName::try_new(name, coll)
                        .map_err(|err| ShutdownActionError::InvalidChildName { err })?;

                    let child_instance_res = self.children.get(moniker);
                    if child_instance_res.is_none() {
                        continue;
                    }
                    child_instance_res.unwrap().clone()
                }
                _ => self.instance.clone(),
            };
            let target: ShutdownComponentInfo<'_> =
                ShutdownComponentInfo { dependency_node, component };
            shutdown_component(target, self.shutdown_type).await?;
        }

        Ok(())
    }
}

pub async fn do_shutdown(
    component: &Arc<ComponentInstance>,
    shutdown_type: ShutdownType,
) -> Result<(), ActionError> {
    const WATCHDOG_INTERVAL: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(15);

    // Keep logs short to preserve as much as possible in the crash report
    // NS: Shutdown of {moniker} was no-op
    // RS: Beginning shutdown of resolved component {moniker}
    // US: Beginning shutdown of unresolved component {moniker}
    // PS: Pending shutdown of component {moniker} has taken more than WATCHDOG_TIMEOUT_SECS
    // FS: Finished shutdown of {moniker}
    // ES: Errored shutdown of {moniker}
    let moniker = component.moniker.clone();
    let _watchdog_task = fasync::Task::spawn(async move {
        let mut interval = fasync::Interval::new(WATCHDOG_INTERVAL);
        while let Some(_) = interval.next().await {
            info!("=PS {}", moniker);
        }
    });
    {
        let state = component.lock_state().await;
        match *state {
            InstanceState::Resolved(ref s) | InstanceState::Started(ref s, _) => {
                if matches!(shutdown_type, ShutdownType::System) {
                    info!("=RS {}", component.moniker);
                }
                let mut shutdown_job = ShutdownJob::new(component, s, shutdown_type).await;
                drop(state);
                Box::pin(shutdown_job.execute()).await.map_err(|err| {
                    warn!("=ES {}", component.moniker);
                    err
                })?;
                if matches!(shutdown_type, ShutdownType::System) {
                    info!("=FS {}", component.moniker);
                }
                return Ok(());
            }
            InstanceState::Shutdown(_, _) => {
                if matches!(shutdown_type, ShutdownType::System) {
                    info!("=NS {}", component.moniker);
                }
                return Ok(());
            }
            InstanceState::Unresolved(_) | InstanceState::Destroyed => {}
        }
    }

    // Control flow arrives here if the component isn't resolved.
    // TODO: Put this component in a "shutting down" state so that if it creates new instances
    // after this point, they are created in a shut down state.
    if let ShutdownType::System = shutdown_type {
        info!("=US {}", component.moniker);
    }

    match component.stop_instance_internal(true).await {
        Ok(()) if shutdown_type == ShutdownType::System => info!("=FS {}", component.moniker),
        Ok(()) => (),
        Err(e) if shutdown_type == ShutdownType::System => {
            info!("=ES {}", component.moniker);
            return Err(e.into());
        }
        Err(e) => return Err(e.into()),
    }

    Ok(())
}

/// Identifies a component in this realm. This can either be the component
/// itself, one of its children, or a capability (used only as an intermediate node for dependency
/// tracking).
#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub enum ComponentRef {
    Self_,
    Child(ChildName),
    // A capability defined by this component (this is either a dictionary or storage capability).
    Capability(Name),
}

impl From<ChildName> for ComponentRef {
    fn from(moniker: ChildName) -> Self {
        Self::Child(moniker)
    }
}

/// Used to track information during the shutdown process. The dependents
/// are all the component which must stop before the component represented
/// by this struct.
struct ShutdownInfo {
    // TODO(jmatt) reduce visibility of fields
    /// The identifier for this component
    pub ref_: ComponentRef,
    /// The components that this component offers capabilities to
    pub dependents: HashSet<ComponentRef>,
    pub component: Arc<ComponentInstance>,
}

impl fmt::Debug for ShutdownInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{server: {{{:?}}}, ", self.component.moniker.to_string())?;
        write!(f, "clients: [")?;
        for dep in &self.dependents {
            write!(f, "{{{:?}}}, ", dep)?;
        }
        write!(f, "]}}")?;
        Ok(())
    }
}

/// Trait exposing all component state necessary to compute shutdown order.
///
/// This trait largely mirrors `ComponentDecl`, but will reflect changes made to
/// the component's state at runtime (e.g., dynamically created children,
/// dynamic offers).
///
/// In production, this will probably only be implemented for
/// `ResolvedInstanceState`, but exposing this trait allows for easier testing.
pub trait Component {
    /// Current view of this component's `uses` declarations.
    fn uses(&self) -> Vec<UseDecl>;

    /// Current view of this component's `exposes` declarations.
    #[allow(dead_code)]
    fn exposes(&self) -> Vec<ExposeDecl>;

    /// Current view of this component's `offers` declarations.
    fn offers(&self) -> Vec<OfferDecl>;

    /// Current view of this component's `capabilities` declarations.
    fn capabilities(&self) -> Vec<CapabilityDecl>;

    /// Current view of this component's `collections` declarations.
    #[allow(dead_code)]
    fn collections(&self) -> Vec<CollectionDecl>;

    /// Current view of this component's `environments` declarations.
    fn environments(&self) -> Vec<EnvironmentDecl>;

    /// Returns metadata about each child of this component.
    fn children(&self) -> Vec<Child>;

    /// Returns the live child that has the given `name` and `collection`, or
    /// returns `None` if none match. In the case of dynamic children, it's
    /// possible for multiple children to match a given `name` and `collection`,
    /// but at most one of them can be live.
    ///
    /// Note: `name` is a `&str` because it could be either a `Name` or `LongName`.
    fn find_child(&self, name: &str, collection: Option<&Name>) -> Option<Child> {
        self.children().into_iter().find(|child| {
            child.moniker.name().as_str() == name
                && child.moniker.collection() == collection.map(Borrow::borrow)
        })
    }
}

/// Child metadata necessary to compute shutdown order.
///
/// A `Component` returns information about its children by returning a vector
/// of these.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Child {
    /// The moniker identifying the name of the child, complete with
    /// `instance_id`.
    pub moniker: ChildName,

    /// Name of the environment associated with this child, if any.
    pub environment_name: Option<Name>,
}

/// For a given component `decl`, identify capability dependencies between the
/// component itself and its children. A directed graph of these dependencies is
/// returned.
pub fn process_deps<'a>(
    decl: &'a fdecl::Component,
    dynamic_children: &Vec<(&'a str, &'a str)>,
    dynamic_offers: &'a Vec<fdecl::Offer>,
) -> directed_graph::DirectedGraph<DependencyNode<'a>> {
    let mut strong_dependencies: DirectedGraph<DependencyNode<'a>> =
        directed_graph::DirectedGraph::new();
    cm_graph::generate_dependency_graph(
        &mut strong_dependencies,
        decl,
        dynamic_children,
        dynamic_offers,
    );
    let self_dep_closure = strong_dependencies.get_closure(DependencyNode::Self_);

    if let Some(children) = decl.children.as_ref() {
        for child in children {
            if let Some(child_name) = child.name.as_ref() {
                let dependency_node = DependencyNode::Child(child_name, None);
                if !self_dep_closure.contains(&dependency_node) {
                    strong_dependencies.add_edge(DependencyNode::Self_, dependency_node);
                }
            }
        }
    }

    for (child_name, collection) in dynamic_children {
        let dependency_node = DependencyNode::Child(child_name, Some(collection));
        if !self_dep_closure.contains(&dependency_node) {
            strong_dependencies.add_edge(DependencyNode::Self_, dependency_node);
        }
    }
    strong_dependencies.add_node(DependencyNode::Self_);
    strong_dependencies
}

/// For a given Component, identify capability dependencies between the
/// component itself and its children. A map is returned which maps from a
/// "source" component (represented by a `ComponentRef`) to a set of "target"
/// components to which the source component provides capabilities. The targets
/// must be shut down before the source.
pub fn process_component_dependencies(
    instance: &impl Component,
) -> HashMap<ComponentRef, HashSet<ComponentRef>> {
    // We build up the set of (source, target) dependency edges from a variety
    // of sources.
    let mut dependencies = Dependencies::new();
    dependencies.extend(get_dependencies_from_offers(instance).into_iter());
    dependencies.extend(get_dependencies_from_environments(instance).into_iter());
    dependencies.extend(get_dependencies_from_uses(instance).into_iter());
    dependencies.extend(get_dependencies_from_capabilities(instance).into_iter());

    // Next, we want to find any children that `self` transitively depends on,
    // either directly or through other children. Any child that `self` doesn't
    // transitively depend on implicitly depends on `self`.
    //
    // TODO(82689): This logic is likely unnecessary, as it deals with children
    // that have no direct dependency links with their parent.
    let self_dependencies_closure = dependency_closure(&dependencies, ComponentRef::Self_);
    let implicit_edges = instance.children().into_iter().filter_map(|child| {
        let component_ref = child.moniker.into();
        if self_dependencies_closure.contains(&component_ref) {
            None
        } else {
            Some((ComponentRef::Self_, component_ref))
        }
    });
    dependencies.extend(implicit_edges);

    dependencies.finalize(instance)
}

/// Given a dependency graph represented as a set of `edges`, find the set of
/// all nodes that the `start` node depends on, directly or indirectly. This
/// includes `start` itself.
fn dependency_closure(edges: &Dependencies, start: ComponentRef) -> HashSet<ComponentRef> {
    let mut res = HashSet::new();
    res.insert(start);
    loop {
        let mut entries_added = false;

        for (source, target) in edges.iter() {
            if !res.contains(target) {
                continue;
            }
            if res.insert(source.clone()) {
                entries_added = true
            }
        }
        if !entries_added {
            return res;
        }
    }
}

/// Data structure used to represent the shutdown dependency graph.
///
/// Once fully constructed, call [Dependencies::normalize] to get the list of shutdown dependency
/// edges as `(ComponentRef, ComponentRef)` pairs.
struct Dependencies {
    inner: HashMap<ComponentRef, HashSet<ComponentRef>>,
}

impl fmt::Debug for Dependencies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.inner)
    }
}

impl Dependencies {
    fn new() -> Self {
        Self { inner: HashMap::new() }
    }

    fn insert(&mut self, k: ComponentRef, v: ComponentRef) {
        self.inner.entry(k).or_insert(HashSet::new()).insert(v);
    }

    fn extend(&mut self, edges: impl Iterator<Item = (ComponentRef, ComponentRef)>) {
        for (k, v) in edges {
            self.insert(k, v);
        }
    }

    fn iter(&self) -> impl Iterator<Item = (&ComponentRef, &ComponentRef)> {
        self.inner.iter().map(|(k, v)| iter::zip(iter::repeat(k), v.iter())).flatten()
    }

    fn into_iter(self) -> impl Iterator<Item = (ComponentRef, ComponentRef)> {
        self.inner.into_iter().map(|(k, v)| iter::zip(iter::repeat(k), v.into_iter())).flatten()
    }

    fn finalize(
        mut self,
        instance: &impl Component,
    ) -> HashMap<ComponentRef, HashSet<ComponentRef>> {
        // To ensure the shutdown job visits all the components in this realm, we need to make sure
        // that every node is in the dependency list even if there were no routes into them.
        self.inner.entry(ComponentRef::Self_).or_insert(HashSet::new());
        for child in instance.children() {
            self.inner.entry(child.moniker.into()).or_insert(HashSet::new());
        }
        for c in instance.capabilities() {
            match c {
                CapabilityDecl::Dictionary(d) => {
                    self.inner.entry(ComponentRef::Capability(d.name)).or_insert(HashSet::new());
                }
                CapabilityDecl::Storage(s) => {
                    self.inner.entry(ComponentRef::Capability(s.name)).or_insert(HashSet::new());
                }
                _ => {}
            }
        }

        self.inner
    }
}

/// Return the set of dependency relationships that can be derived from the
/// component's use declarations. For use declarations, `self` is always the
/// target.
fn get_dependencies_from_uses(instance: &impl Component) -> Dependencies {
    let mut edges = Dependencies::new();
    for use_ in &instance.uses() {
        match use_ {
            UseDecl::Protocol(UseProtocolDecl { dependency_type, .. })
            | UseDecl::Service(UseServiceDecl { dependency_type, .. })
            | UseDecl::Directory(UseDirectoryDecl { dependency_type, .. }) => {
                if dependency_type == &DependencyType::Weak {
                    // Weak dependencies are ignored when determining shutdown ordering
                    continue;
                }
            }
            UseDecl::Storage(_)
            | UseDecl::EventStream(_)
            | UseDecl::Runner(_)
            | UseDecl::Config(_) => {
                // Any other capability type cannot be marked as weak, so we can proceed
            }
        }
        let deps = match use_ {
            UseDecl::Service(UseServiceDecl { source, .. })
            | UseDecl::Protocol(UseProtocolDecl { source, .. })
            | UseDecl::Directory(UseDirectoryDecl { source, .. })
            | UseDecl::EventStream(UseEventStreamDecl { source, .. })
            | UseDecl::Config(UseConfigurationDecl { source, .. })
            | UseDecl::Runner(UseRunnerDecl { source, .. }) => match source {
                UseSource::Child(name) => instance
                    .find_child(name.as_str(), None)
                    .map(|child| vec![ComponentRef::from(child.moniker)]),
                UseSource::Self_ => {
                    if use_.is_from_dictionary() {
                        let path = use_.source_path();
                        let dictionary =
                            path.iter_segments().next().expect("must contain at least one segment");
                        Some(vec![ComponentRef::Capability(dictionary.into())])
                    } else {
                        // Self is the other node, no need to add a loop.
                        None
                    }
                }
                UseSource::Collection(collection_name) => Some(
                    instance
                        .children()
                        .into_iter()
                        .filter(|child_instance| {
                            // We only care about children that are in a collection with a name
                            // matching `collection_name`.
                            child_instance
                                .moniker
                                .collection()
                                .map(|c| c == collection_name)
                                .unwrap_or(false)
                        })
                        .map(|child_instance| child_instance.moniker.into())
                        .collect(),
                ),
                _ => None,
            },
            UseDecl::Storage(UseStorageDecl { .. }) => {
                // source is always parent.
                None
            }
        };
        if let Some(deps) = deps {
            for dep in deps {
                edges.insert(dep, ComponentRef::Self_);
            }
        }
    }
    edges
}

/// Return the set of dependency relationships that can be derived from the
/// component's offer declarations. This includes both static and dynamic offers.
fn get_dependencies_from_offers(instance: &impl Component) -> Dependencies {
    let mut edges = Dependencies::new();
    for offer_decl in instance.offers() {
        if let Some((sources, targets)) = get_dependency_from_offer(instance, &offer_decl) {
            for source in sources.iter() {
                for target in targets.iter() {
                    edges.insert(source.clone(), target.clone());
                }
            }
        }
    }
    edges
}

/// Extracts a list of sources and a list of targets from a single `OfferDecl`,
/// or returns `None` if the offer has no impact on shutdown ordering. The
/// `Component` provides context that may be necessary to understand the
/// `OfferDecl`. Note that a single offer can have multiple sources/targets; for
/// instance, targeting a collection targets all the children within that
/// collection.
fn get_dependency_from_offer(
    instance: &impl Component,
    offer_decl: &OfferDecl,
) -> Option<(Vec<ComponentRef>, Vec<ComponentRef>)> {
    // We only care about dependencies where the provider of the dependency is
    // `self` or another child, otherwise the capability comes from the parent
    // or component manager itself in which case the relationship is not
    // relevant for ordering here.
    match offer_decl {
        OfferDecl::Protocol(OfferProtocolDecl {
            dependency_type: DependencyType::Strong,
            source,
            target,
            ..
        })
        | OfferDecl::Directory(OfferDirectoryDecl {
            dependency_type: DependencyType::Strong,
            source,
            target,
            ..
        })
        | OfferDecl::Service(OfferServiceDecl {
            dependency_type: DependencyType::Strong,
            source,
            target,
            ..
        })
        | OfferDecl::Config(OfferConfigurationDecl { source, target, .. })
        | OfferDecl::Runner(OfferRunnerDecl { source, target, .. })
        | OfferDecl::Resolver(OfferResolverDecl { source, target, .. })
        | OfferDecl::Storage(OfferStorageDecl { source, target, .. })
        | OfferDecl::Dictionary(OfferDictionaryDecl {
            dependency_type: DependencyType::Strong,
            source,
            target,
            ..
        }) => Some((
            find_offer_sources(offer_decl, instance, source),
            find_offer_targets(instance, target),
        )),

        OfferDecl::Protocol(OfferProtocolDecl {
            dependency_type: DependencyType::Weak, ..
        })
        | OfferDecl::Service(OfferServiceDecl { dependency_type: DependencyType::Weak, .. })
        | OfferDecl::Directory(OfferDirectoryDecl {
            dependency_type: DependencyType::Weak, ..
        })
        | OfferDecl::Dictionary(OfferDictionaryDecl {
            dependency_type: DependencyType::Weak,
            ..
        }) => {
            // weak dependencies are ignored by this algorithm, because weak
            // dependencies can be broken arbitrarily.
            None
        }

        OfferDecl::EventStream(_) => {
            // Event streams aren't tracked as dependencies for shutdown.
            None
        }
    }
}

/// Given a `Component` instance and an `OfferSource`, return the names of
/// components that match that `source`.
fn find_offer_sources(
    offer: &OfferDecl,
    instance: &impl Component,
    source: &OfferSource,
) -> Vec<ComponentRef> {
    match source {
        OfferSource::Child(ChildRef { name, collection }) => {
            match instance.find_child(name.as_str(), collection.as_ref()) {
                Some(child) => vec![child.moniker.into()],
                None => {
                    error!(
                        "offer source doesn't exist: (name: {:?}, collection: {:?})",
                        name, collection
                    );
                    vec![]
                }
            }
        }
        OfferSource::Self_ => {
            if offer.is_from_dictionary()
                || matches!(offer, OfferDecl::Dictionary(_))
                || matches!(offer, OfferDecl::Storage(_))
            {
                let path = offer.source_path();
                let name = path.iter_segments().next().expect("must contain at least one segment");
                vec![ComponentRef::Capability(name.into())]
            } else {
                vec![ComponentRef::Self_]
            }
        }
        OfferSource::Collection(collection_name) => instance
            .children()
            .into_iter()
            .filter_map(|child| {
                if let Some(child_collection) = child.moniker.collection() {
                    if child_collection == collection_name {
                        Some(child.moniker.clone().into())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect(),
        OfferSource::Parent | OfferSource::Framework => {
            // Capabilities offered by the parent or provided by the framework
            // (based on some other capability) are not relevant.
            vec![]
        }
        OfferSource::Capability(_) => {
            // OfferSource::Capability(_) is used for the `StorageAdmin`
            // capability. This capability is implemented by component_manager,
            // and is therefore similar to a Framework capability, but it also
            // depends on the underlying storage that's being administrated. In
            // theory, we could add an edge from the source of that storage to
            // the target of the`StorageAdmin` capability... but honestly it's
            // very complex and confusing and doesn't seem worth it.
            //
            // We may want to reconsider this someday.
            vec![]
        }
        OfferSource::Void => {
            // Offer sources that are intentionally omitted will never match any components
            vec![]
        }
    }
}

/// Given a `Component` instance and an `OfferTarget`, return the names of
/// components that match that `target`.
fn find_offer_targets(instance: &impl Component, target: &OfferTarget) -> Vec<ComponentRef> {
    match target {
        OfferTarget::Child(ChildRef { name, collection }) => {
            match instance.find_child(name.as_str(), collection.as_ref()) {
                Some(child) => vec![child.moniker.into()],
                None => {
                    error!(
                        "offer target doesn't exist: (name: {:?}, collection: {:?})",
                        name, collection
                    );
                    vec![]
                }
            }
        }
        OfferTarget::Collection(collection) => instance
            .children()
            .into_iter()
            .filter(|child| child.moniker.collection() == Some(collection))
            .map(|child| child.moniker.into())
            .collect(),
        OfferTarget::Capability(capability) => {
            // `capability` must be a dictionary.
            vec![ComponentRef::Capability(capability.clone())]
        }
    }
}

/// Return the set of dependency relationships that can be derived from the
/// component's environment configuration. Children assigned to an environment
/// depend on components that contribute to that environment.
fn get_dependencies_from_environments(instance: &impl Component) -> Dependencies {
    let env_to_sources: HashMap<Name, HashSet<ComponentRef>> = instance
        .environments()
        .into_iter()
        .map(|env| {
            let sources = find_environment_sources(instance, &env);
            (env.name, sources)
        })
        .collect();

    let mut edges = Dependencies::new();
    for child in &instance.children() {
        if let Some(env_name) = &child.environment_name {
            if let Some(source_children) = env_to_sources.get(env_name) {
                for source in source_children {
                    edges.insert(source.clone(), child.moniker.clone().into());
                }
            } else {
                error!(
                    "environment `{}` from child `{}` is not a valid environment",
                    env_name, child.moniker
                )
            }
        }
    }
    edges
}

/// Return the set of dependency relationships that can be derived from the
/// component's capabilities declarations. For use declarations, `self` is always the
/// target.
fn get_dependencies_from_capabilities(instance: &impl Component) -> Dependencies {
    let mut edges = Dependencies::new();
    for capability in &instance.capabilities() {
        match capability {
            CapabilityDecl::Dictionary(DictionaryDecl { name, source_path, .. }) => {
                if let Some(_) = source_path {
                    // Dictionary is backed by the program, so there is an effective dependency
                    // from Self.
                    edges.insert(ComponentRef::Self_, ComponentRef::Capability(name.clone()));
                }
            }
            CapabilityDecl::Storage(StorageDecl { name, source, .. }) => {
                let source = match source {
                    StorageDirectorySource::Parent => None,
                    StorageDirectorySource::Child(name) => {
                        match instance.find_child(name.as_str(), None) {
                            Some(child) => Some(child.moniker.clone().into()),
                            None => {
                                error!("storage source doesn't exist: (name: {:?})", name);
                                None
                            }
                        }
                    }
                    StorageDirectorySource::Self_ => Some(ComponentRef::Self_),
                };
                if let Some(source) = source {
                    edges.insert(source, ComponentRef::Capability(name.clone()));
                }
            }
            _ => {}
        }
    }
    edges
}

/// Given a `Component` instance and an environment, return the names of
/// components that provide runners, resolvers, or debug_capabilities for that
/// environment.
fn find_environment_sources(
    instance: &impl Component,
    env: &EnvironmentDecl,
) -> HashSet<ComponentRef> {
    // Get all the `RegistrationSources` for the runners, resolvers, and
    // debug_capabilities in this environment.
    let registration_sources = env
        .runners
        .iter()
        .map(|r| &r.source)
        .chain(env.resolvers.iter().map(|r| &r.source))
        .chain(env.debug_capabilities.iter().map(|r| r.source()));

    // Turn the shutdown-relevant sources into `ComponentRef`s.
    registration_sources
        .flat_map(|source| match source {
            RegistrationSource::Self_ => vec![ComponentRef::Self_],
            RegistrationSource::Child(child_name) => {
                match instance.find_child(child_name.as_str(), None) {
                    Some(child) => vec![child.moniker.into()],
                    None => {
                        error!(
                            "source for environment {:?} doesn't exist: (name: {:?})",
                            env.name, child_name,
                        );
                        vec![]
                    }
                }
            }
            RegistrationSource::Parent => vec![],
        })
        .collect()
}

#[cfg(all(test, not(feature = "src_model_tests")))]
mod tests {
    use super::*;
    use crate::model::actions::test_utils::MockAction;
    use crate::model::actions::StopAction;
    use crate::model::component::StartReason;
    use crate::model::testing::out_dir::OutDir;
    use crate::model::testing::test_helpers::{
        component_decl_with_test_runner, execution_is_shut_down, has_child, ActionsTest,
        ComponentInfo,
    };
    use crate::model::testing::test_hook::Lifecycle;
    use async_utils::PollExt;
    use cm_rust::{ComponentDecl, DependencyType, ExposeSource, ExposeTarget};
    use cm_rust_testing::*;
    use cm_types::AllowedOffers;
    use errors::StopActionError;
    use fidl::endpoints::RequestStream;
    use maplit::{btreeset, hashmap, hashset};
    use moniker::Moniker;
    use std::collections::BTreeSet;
    use test_case::test_case;
    use {
        fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
        fidl_fuchsia_component_runner as fcrunner, fuchsia_async as fasync,
    };

    /// Implementation of `super::Component` based on a `ComponentDecl` and
    /// minimal information about runtime state.
    struct FakeComponent {
        decl: ComponentDecl,
        dynamic_children: Vec<Child>,
        dynamic_offers: Vec<OfferDecl>,
    }

    impl FakeComponent {
        /// Returns a `FakeComponent` with no dynamic children or offers.
        fn from_decl(decl: ComponentDecl) -> Self {
            Self { decl, dynamic_children: vec![], dynamic_offers: vec![] }
        }
    }

    impl Component for FakeComponent {
        fn uses(&self) -> Vec<UseDecl> {
            self.decl.uses.clone()
        }

        fn exposes(&self) -> Vec<ExposeDecl> {
            self.decl.exposes.clone()
        }

        fn offers(&self) -> Vec<OfferDecl> {
            self.decl.offers.iter().cloned().chain(self.dynamic_offers.iter().cloned()).collect()
        }

        fn capabilities(&self) -> Vec<CapabilityDecl> {
            self.decl.capabilities.clone()
        }

        fn collections(&self) -> Vec<cm_rust::CollectionDecl> {
            self.decl.collections.clone()
        }

        fn environments(&self) -> Vec<cm_rust::EnvironmentDecl> {
            self.decl.environments.clone()
        }

        fn children(&self) -> Vec<Child> {
            self.decl
                .children
                .iter()
                .map(|c| Child {
                    moniker: ChildName::try_new(c.name.as_str(), None)
                        .expect("children should have valid monikers"),
                    environment_name: c.environment.clone(),
                })
                .chain(self.dynamic_children.iter().cloned())
                .collect()
        }
    }

    // TODO(jmatt) Add tests for all capability types

    /// Returns a `ComponentRef` for a child by parsing the moniker. Panics if
    /// the moniker is malformed.
    fn child(moniker: &str) -> ComponentRef {
        ChildName::try_from(moniker).unwrap().into()
    }

    fn capability(name: &str) -> ComponentRef {
        ComponentRef::Capability(name.parse().unwrap())
    }

    #[fuchsia::test]
    fn test_service_from_self() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("serviceSelf")
                    .source(OfferSource::Self_)
                    .target_static_child("childA"),
            )
            .child_default("childA")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA")],
                child("childA") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> =
            vec![DependencyNode::Child("childA", None), DependencyNode::Self_];

        assert_eq!(ans, sorted_map)
    }

    #[test_case(DependencyType::Weak)]
    fn test_weak_service_from_self(weak_dep: DependencyType) {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("serviceSelf")
                    .source(OfferSource::Self_)
                    .target_static_child("childA")
                    .dependency(weak_dep),
            )
            .child_default("childA")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA")],
                child("childA") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> =
            vec![DependencyNode::Child("childA", None), DependencyNode::Self_];

        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_service_from_child() {
        let decl = ComponentDeclBuilder::new()
            .expose(
                ExposeBuilder::protocol()
                    .name("serviceFromChild")
                    .source_static_child("childA")
                    .target(ExposeTarget::Parent),
            )
            .child_default("childA")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA")],
                child("childA") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> =
            vec![DependencyNode::Child("childA", None), DependencyNode::Self_];

        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_single_dependency() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("serviceParent")
                    .source(OfferSource::Self_)
                    .target_static_child("childA"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childBOffer")
                    .target_name("serviceSibling")
                    .source_static_child("childB")
                    .target_static_child("childA"),
            )
            .child_default("childA")
            .child_default("childB")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB")],
                child("childA") => hashset![],
                child("childB") => hashset![child("childA")],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
        ];

        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_dictionary_dependency() {
        let decl = ComponentDeclBuilder::new()
            .dictionary_default("dict")
            .protocol_default("serviceA")
            .offer(
                OfferBuilder::protocol()
                    .name("serviceA")
                    .source(OfferSource::Self_)
                    .target(OfferTarget::Capability("dict".parse().unwrap())),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("serviceB")
                    .source_static_child("childA")
                    .target(OfferTarget::Capability("dict".parse().unwrap())),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("serviceB")
                    .source(OfferSource::Self_)
                    .from_dictionary("dict")
                    .target_static_child("childB"),
            )
            .child_default("childA")
            .child_default("childB")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB"), capability("dict")],
                child("childA") => hashset![capability("dict")],
                capability("dict") => hashset![child("childB")],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childB", None),
            DependencyNode::Capability("dict"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];

        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_environment_with_runner_from_parent() {
        let decl = ComponentDeclBuilder::new()
            .environment(EnvironmentBuilder::new().name("env").runner(
                cm_rust::RunnerRegistration {
                    source: RegistrationSource::Parent,
                    source_name: "foo".parse().unwrap(),
                    target_name: "foo".parse().unwrap(),
                },
            ))
            .child_default("childA")
            .child(ChildBuilder::new().name("childB").environment("env"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB")],
                child("childA") => hashset![],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
            DependencyNode::Environment("env"),
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_environment_with_runner_from_self() {
        let decl = ComponentDeclBuilder::new()
            .environment(EnvironmentBuilder::new().name("env").runner(
                cm_rust::RunnerRegistration {
                    source: RegistrationSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    target_name: "foo".parse().unwrap(),
                },
            ))
            .child_default("childA")
            .child(ChildBuilder::new().name("childB").environment("env"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB")],
                child("childA") => hashset![],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Environment("env"),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_environment_with_runner_from_child() {
        let decl = ComponentDeclBuilder::new()
            .environment(EnvironmentBuilder::new().name("env").runner(
                cm_rust::RunnerRegistration {
                    source: RegistrationSource::Child("childA".to_string()),
                    source_name: "foo".parse().unwrap(),
                    target_name: "foo".parse().unwrap(),
                },
            ))
            .child_default("childA")
            .child(ChildBuilder::new().name("childB").environment("env"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB")],
                child("childA") => hashset![child("childB")],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childB", None),
            DependencyNode::Environment("env"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_environment_with_runner_from_child_to_collection() {
        let decl = ComponentDeclBuilder::new()
            .environment(EnvironmentBuilder::new().name("env").runner(
                cm_rust::RunnerRegistration {
                    source: RegistrationSource::Child("childA".to_string()),
                    source_name: "foo".parse().unwrap(),
                    target_name: "foo".parse().unwrap(),
                },
            ))
            .collection(CollectionBuilder::new().name("coll").environment("env"))
            .child_default("childA")
            .build();

        let instance = FakeComponent {
            decl: decl.clone(),
            dynamic_children: vec![
                // NOTE: The environment must be set in the `Child`, even though
                // it can theoretically be inferred from the collection
                // declaration.
                Child {
                    moniker: "coll:dyn1".try_into().unwrap(),
                    environment_name: Some("env".parse().unwrap()),
                },
                Child {
                    moniker: "coll:dyn2".try_into().unwrap(),
                    environment_name: Some("env".parse().unwrap()),
                },
            ],
            dynamic_offers: vec![],
        };

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("coll:dyn1"),
                    child("coll:dyn2"),
                ],
                child("childA") => hashset![
                    child("coll:dyn1"),
                    child("coll:dyn2"),
                ],
                child("coll:dyn1") => hashset![],
                child("coll:dyn2") => hashset![],
            },
            process_component_dependencies(&instance)
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![("dyn1", "coll"), ("dyn2", "coll")], &dynamic_offers)
                .topological_sort()
                .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("dyn1", Some("coll")),
            DependencyNode::Child("dyn2", Some("coll")),
            DependencyNode::Collection("coll"),
            DependencyNode::Environment("env"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_chained_environments() {
        let decl = ComponentDeclBuilder::new()
            .environment(EnvironmentBuilder::new().name("env").runner(
                cm_rust::RunnerRegistration {
                    source: RegistrationSource::Child("childA".to_string()),
                    source_name: "foo".parse().unwrap(),
                    target_name: "foo".parse().unwrap(),
                },
            ))
            .environment(EnvironmentBuilder::new().name("env2").runner(
                cm_rust::RunnerRegistration {
                    source: RegistrationSource::Child("childB".to_string()),
                    source_name: "bar".parse().unwrap(),
                    target_name: "bar".parse().unwrap(),
                },
            ))
            .child_default("childA")
            .child(ChildBuilder::new().name("childB").environment("env"))
            .child(ChildBuilder::new().name("childC").environment("env2"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("childB"),
                    child("childC")
                ],
                child("childA") => hashset![child("childB")],
                child("childB") => hashset![child("childC")],
                child("childC") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childC", None),
            DependencyNode::Environment("env2"),
            DependencyNode::Child("childB", None),
            DependencyNode::Environment("env"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_environment_and_offer() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("childBOffer")
                    .target_name("serviceSibling")
                    .source_static_child("childB")
                    .target_static_child("childC"),
            )
            .environment(EnvironmentBuilder::new().name("env").runner(
                cm_rust::RunnerRegistration {
                    source: RegistrationSource::Child("childA".into()),
                    source_name: "foo".parse().unwrap(),
                    target_name: "foo".parse().unwrap(),
                },
            ))
            .child_default("childA")
            .child(ChildBuilder::new().name("childB").environment("env"))
            .child_default("childC")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("childB"),
                    child("childC")
                ],
                child("childA") => hashset![child("childB")],
                child("childB") => hashset![child("childC")],
                child("childC") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childC", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Environment("env"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_environment_with_resolver_from_parent() {
        let decl = ComponentDeclBuilder::new()
            .environment(EnvironmentBuilder::new().name("resolver_env").resolver(
                cm_rust::ResolverRegistration {
                    source: RegistrationSource::Parent,
                    resolver: "foo".parse().unwrap(),
                    scheme: "httweeeeees".into(),
                },
            ))
            .child_default("childA")
            .child(ChildBuilder::new().name("childB").environment("resolver_env"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB")],
                child("childA") => hashset![],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
            DependencyNode::Environment("resolver_env"),
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_environment_with_resolver_from_child() {
        let decl = ComponentDeclBuilder::new()
            .environment(EnvironmentBuilder::new().name("resolver_env").resolver(
                cm_rust::ResolverRegistration {
                    source: RegistrationSource::Child("childA".to_string()),
                    resolver: "foo".parse().unwrap(),
                    scheme: "httweeeeees".into(),
                },
            ))
            .child_default("childA")
            .child(ChildBuilder::new().name("childB").environment("resolver_env"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB")],
                child("childA") => hashset![child("childB")],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childB", None),
            DependencyNode::Environment("resolver_env"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    // add test where B depends on A via environment and C depends on B via environment

    #[fuchsia::test]
    fn test_environment_with_chain_of_resolvers() {
        let decl = ComponentDeclBuilder::new()
            .environment(EnvironmentBuilder::new().name("env1").resolver(
                cm_rust::ResolverRegistration {
                    source: RegistrationSource::Child("childA".to_string()),
                    resolver: "foo".parse().unwrap(),
                    scheme: "httweeeeees".into(),
                },
            ))
            .environment(EnvironmentBuilder::new().name("env2").resolver(
                cm_rust::ResolverRegistration {
                    source: RegistrationSource::Child("childB".to_string()),
                    resolver: "bar".parse().unwrap(),
                    scheme: "httweeeeee".into(),
                },
            ))
            .child_default("childA")
            .child(ChildBuilder::new().name("childB").environment("env1"))
            .child(ChildBuilder::new().name("childC").environment("env2"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("childB"),
                    child("childC")
                ],
                child("childA") => hashset![child("childB")],
                child("childB") => hashset![child("childC")],
                child("childC") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childC", None),
            DependencyNode::Environment("env2"),
            DependencyNode::Child("childB", None),
            DependencyNode::Environment("env1"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_environment_with_resolver_and_runner_from_child() {
        let decl = ComponentDeclBuilder::new()
            .environment(
                EnvironmentBuilder::new()
                    .name("multi_env")
                    .resolver(cm_rust::ResolverRegistration {
                        source: RegistrationSource::Child("childA".to_string()),
                        resolver: "foo".parse().unwrap(),
                        scheme: "httweeeeees".into(),
                    })
                    .runner(cm_rust::RunnerRegistration {
                        source: RegistrationSource::Child("childB".to_string()),
                        source_name: "bar".parse().unwrap(),
                        target_name: "bar".parse().unwrap(),
                    }),
            )
            .child_default("childA")
            .child_default("childB")
            .child(ChildBuilder::new().name("childC").environment("multi_env"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("childB"),
                    child("childC")
                ],
                child("childA") => hashset![child("childC")],
                child("childB") => hashset![child("childC")],
                child("childC") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childC", None),
            DependencyNode::Environment("multi_env"),
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_environment_with_collection_resolver_from_child() {
        let decl = ComponentDeclBuilder::new()
            .environment(EnvironmentBuilder::new().name("resolver_env").resolver(
                cm_rust::ResolverRegistration {
                    source: RegistrationSource::Child("childA".to_string()),
                    resolver: "foo".parse().unwrap(),
                    scheme: "httweeeeees".into(),
                },
            ))
            .child_default("childA")
            .collection(CollectionBuilder::new().name("coll").environment("resolver_env"))
            .build();

        let instance = FakeComponent {
            decl: decl.clone(),
            dynamic_children: vec![
                // NOTE: The environment must be set in the `Child`, even though
                // it can theoretically be inferred from the collection declaration.
                Child {
                    moniker: "coll:dyn1".try_into().unwrap(),
                    environment_name: Some("resolver_env".parse().unwrap()),
                },
                Child {
                    moniker: "coll:dyn2".try_into().unwrap(),
                    environment_name: Some("resolver_env".parse().unwrap()),
                },
            ],
            dynamic_offers: vec![],
        };

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("coll:dyn1"),
                    child("coll:dyn2"),
                ],
                child("childA") => hashset![child("coll:dyn1"), child("coll:dyn2")],
                child("coll:dyn1") => hashset![],
                child("coll:dyn2") => hashset![],
            },
            process_component_dependencies(&instance)
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![("dyn1", "coll"), ("dyn2", "coll")], &dynamic_offers)
                .topological_sort()
                .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("dyn1", Some("coll")),
            DependencyNode::Child("dyn2", Some("coll")),
            DependencyNode::Collection("coll"),
            DependencyNode::Environment("resolver_env"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_dynamic_offers_within_collection() {
        let decl = ComponentDeclBuilder::new()
            .child_default("childA")
            .collection_default("coll")
            .offer(
                OfferBuilder::directory()
                    .name("some_dir")
                    .source(OfferSource::Child(ChildRef {
                        name: "childA".parse().unwrap(),
                        collection: None,
                    }))
                    .target(OfferTarget::Collection("coll".parse().unwrap())),
            )
            .build();

        let instance = FakeComponent {
            decl: decl.clone(),
            dynamic_children: vec![
                Child { moniker: "coll:dyn1".try_into().unwrap(), environment_name: None },
                Child { moniker: "coll:dyn2".try_into().unwrap(), environment_name: None },
                Child { moniker: "coll:dyn3".try_into().unwrap(), environment_name: None },
                Child { moniker: "coll:dyn4".try_into().unwrap(), environment_name: None },
            ],
            dynamic_offers: vec![
                OfferBuilder::protocol()
                    .name("test.protocol")
                    .target_name("test.protocol")
                    .source(OfferSource::Child(ChildRef {
                        name: "dyn1".parse().unwrap(),
                        collection: Some("coll".parse().unwrap()),
                    }))
                    .target(OfferTarget::Child(ChildRef {
                        name: "dyn2".parse().unwrap(),
                        collection: Some("coll".parse().unwrap()),
                    }))
                    .build(),
                OfferBuilder::protocol()
                    .name("test.protocol")
                    .target_name("test.protocol")
                    .source(OfferSource::Child(ChildRef {
                        name: "dyn1".parse().unwrap(),
                        collection: Some("coll".parse().unwrap()),
                    }))
                    .target(OfferTarget::Child(ChildRef {
                        name: "dyn3".parse().unwrap(),
                        collection: Some("coll".parse().unwrap()),
                    }))
                    .build(),
            ],
        };

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("coll:dyn1"),
                    child("coll:dyn2"),
                    child("coll:dyn3"),
                    child("coll:dyn4"),
                ],
                child("childA") => hashset![
                    child("coll:dyn1"),
                    child("coll:dyn2"),
                    child("coll:dyn3"),
                    child("coll:dyn4"),
                ],
                child("coll:dyn1") => hashset![child("coll:dyn2"), child("coll:dyn3")],
                child("coll:dyn2") => hashset![],
                child("coll:dyn3") => hashset![],
                child("coll:dyn4") => hashset![],
            },
            process_component_dependencies(&instance)
        );

        let fidl_decl = decl.into();
        let offer_1 = fdecl::Offer::Protocol(fdecl::OfferProtocol {
            source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn1".into(),
                collection: Some("coll".parse().unwrap()),
            })),
            target: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn2".into(),
                collection: Some("coll".parse().unwrap()),
            })),
            target_name: Some("test.protocol".to_string()),
            dependency_type: Some(fdecl::DependencyType::Strong),
            ..Default::default()
        });

        let offer_2 = fdecl::Offer::Protocol(fdecl::OfferProtocol {
            source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn1".into(),
                collection: Some("coll".parse().unwrap()),
            })),
            target: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn3".into(),
                collection: Some("coll".parse().unwrap()),
            })),
            target_name: Some("test.protocol".to_string()),
            dependency_type: Some(fdecl::DependencyType::Strong),
            ..Default::default()
        });

        let dynamic_offers = vec![offer_1, offer_2];
        let sorted_map = process_deps(
            &fidl_decl,
            &vec![("dyn1", "coll"), ("dyn2", "coll"), ("dyn3", "coll"), ("dyn4", "coll")],
            &dynamic_offers,
        )
        .topological_sort()
        .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("dyn2", Some("coll")),
            DependencyNode::Child("dyn3", Some("coll")),
            DependencyNode::Child("dyn1", Some("coll")),
            DependencyNode::Child("dyn4", Some("coll")),
            DependencyNode::Collection("coll"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_dynamic_offers_between_collections() {
        let decl = ComponentDeclBuilder::new()
            .collection_default("coll1")
            .collection_default("coll2")
            .build();

        let instance = FakeComponent {
            decl: decl.clone(),
            dynamic_children: vec![
                Child { moniker: "coll1:dyn1".try_into().unwrap(), environment_name: None },
                Child { moniker: "coll1:dyn2".try_into().unwrap(), environment_name: None },
                Child { moniker: "coll2:dyn1".try_into().unwrap(), environment_name: None },
                Child { moniker: "coll2:dyn2".try_into().unwrap(), environment_name: None },
            ],
            dynamic_offers: vec![
                OfferBuilder::protocol()
                    .name("test.protocol")
                    .source(OfferSource::Child(ChildRef {
                        name: "dyn1".parse().unwrap(),
                        collection: Some("coll1".parse().unwrap()),
                    }))
                    .target(OfferTarget::Child(ChildRef {
                        name: "dyn1".parse().unwrap(),
                        collection: Some("coll2".parse().unwrap()),
                    }))
                    .build(),
                OfferBuilder::protocol()
                    .name("test.protocol")
                    .source(OfferSource::Child(ChildRef {
                        name: "dyn2".parse().unwrap(),
                        collection: Some("coll2".parse().unwrap()),
                    }))
                    .target(OfferTarget::Child(ChildRef {
                        name: "dyn1".parse().unwrap(),
                        collection: Some("coll1".parse().unwrap()),
                    }))
                    .build(),
            ],
        };

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("coll1:dyn1"),
                    child("coll1:dyn2"),
                    child("coll2:dyn1"),
                    child("coll2:dyn2"),
                ],
                child("coll1:dyn1") => hashset![child("coll2:dyn1")],
                child("coll1:dyn2") => hashset![],
                child("coll2:dyn1") => hashset![],
                child("coll2:dyn2") => hashset![child("coll1:dyn1")],
            },
            process_component_dependencies(&instance)
        );

        let fidl_decl = decl.into();
        let offer_1 = fdecl::Offer::Protocol(fdecl::OfferProtocol {
            source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn1".into(),
                collection: Some("coll1".parse().unwrap()),
            })),
            target: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn1".into(),
                collection: Some("coll2".parse().unwrap()),
            })),
            target_name: Some("test.protocol".to_string()),
            dependency_type: Some(fdecl::DependencyType::Strong),
            ..Default::default()
        });

        let offer_2 = fdecl::Offer::Protocol(fdecl::OfferProtocol {
            source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn2".into(),
                collection: Some("coll2".parse().unwrap()),
            })),
            target: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn1".into(),
                collection: Some("coll1".parse().unwrap()),
            })),
            target_name: Some("test.protocol".to_string()),
            dependency_type: Some(fdecl::DependencyType::Strong),
            ..Default::default()
        });

        let dynamic_offers = vec![offer_1, offer_2];
        let sorted_map = process_deps(
            &fidl_decl,
            &vec![("dyn1", "coll1"), ("dyn2", "coll1"), ("dyn1", "coll2"), ("dyn2", "coll2")],
            &dynamic_offers,
        )
        .topological_sort()
        .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("dyn1", Some("coll2")),
            DependencyNode::Child("dyn1", Some("coll1")),
            DependencyNode::Child("dyn2", Some("coll1")),
            DependencyNode::Child("dyn2", Some("coll2")),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_dynamic_offer_from_parent() {
        let decl = ComponentDeclBuilder::new().collection_default("coll").build();
        let instance = FakeComponent {
            decl: decl.clone(),
            dynamic_children: vec![
                Child { moniker: "coll:dyn1".try_into().unwrap(), environment_name: None },
                Child { moniker: "coll:dyn2".try_into().unwrap(), environment_name: None },
            ],
            dynamic_offers: vec![OfferBuilder::protocol()
                .name("test.protocol")
                .source(OfferSource::Parent)
                .target(OfferTarget::Child(ChildRef {
                    name: "dyn1".parse().unwrap(),
                    collection: Some("coll".parse().unwrap()),
                }))
                .build()],
        };

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("coll:dyn1"),
                    child("coll:dyn2"),
                ],
                child("coll:dyn1") => hashset![],
                child("coll:dyn2") => hashset![],
            },
            process_component_dependencies(&instance)
        );

        let fidl_decl = decl.into();
        let offer_1 = fdecl::Offer::Protocol(fdecl::OfferProtocol {
            source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
            target: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn1".into(),
                collection: Some("coll".parse().unwrap()),
            })),
            target_name: Some("test.protocol".to_string()),
            dependency_type: Some(fdecl::DependencyType::Strong),
            ..Default::default()
        });

        let dynamic_offers = vec![offer_1];
        let sorted_map =
            process_deps(&fidl_decl, &vec![("dyn1", "coll"), ("dyn2", "coll")], &dynamic_offers)
                .topological_sort()
                .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("dyn1", Some("coll")),
            DependencyNode::Child("dyn2", Some("coll")),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_dynamic_offer_from_self() {
        let decl = ComponentDeclBuilder::new().collection_default("coll").build();
        let instance = FakeComponent {
            decl: decl.clone(),
            dynamic_children: vec![
                Child { moniker: "coll:dyn1".try_into().unwrap(), environment_name: None },
                Child { moniker: "coll:dyn2".try_into().unwrap(), environment_name: None },
            ],
            dynamic_offers: vec![OfferBuilder::protocol()
                .name("test.protocol")
                .source(OfferSource::Self_)
                .target(OfferTarget::Child(ChildRef {
                    name: "dyn1".parse().unwrap(),
                    collection: Some("coll".parse().unwrap()),
                }))
                .build()],
        };

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("coll:dyn1"),
                    child("coll:dyn2"),
                ],
                child("coll:dyn1") => hashset![],
                child("coll:dyn2") => hashset![],
            },
            process_component_dependencies(&instance)
        );

        let fidl_decl = decl.into();
        let offer_1 = fdecl::Offer::Protocol(fdecl::OfferProtocol {
            source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
            target: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn1".into(),
                collection: Some("coll".parse().unwrap()),
            })),
            target_name: Some("test.protocol".to_string()),
            dependency_type: Some(fdecl::DependencyType::Strong),
            ..Default::default()
        });

        let dynamic_offers = vec![offer_1];
        let sorted_map =
            process_deps(&fidl_decl, &vec![("dyn1", "coll"), ("dyn2", "coll")], &dynamic_offers)
                .topological_sort()
                .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("dyn1", Some("coll")),
            DependencyNode::Child("dyn2", Some("coll")),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_dynamic_offer_from_static_child() {
        let decl = ComponentDeclBuilder::new()
            .child_default("childA")
            .child_default("childB")
            .collection_default("coll")
            .build();

        let instance = FakeComponent {
            decl: decl.clone(),
            dynamic_children: vec![
                Child { moniker: "coll:dyn1".try_into().unwrap(), environment_name: None },
                Child { moniker: "coll:dyn2".try_into().unwrap(), environment_name: None },
            ],
            dynamic_offers: vec![OfferBuilder::protocol()
                .name("test.protocol")
                .source_static_child("childA")
                .target(OfferTarget::Child(ChildRef {
                    name: "dyn1".parse().unwrap(),
                    collection: Some("coll".parse().unwrap()),
                }))
                .build()],
        };

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("childB"),
                    child("coll:dyn1"),
                    child("coll:dyn2"),
                ],
                child("childA") => hashset![child("coll:dyn1")],
                child("childB") => hashset![],
                child("coll:dyn1") => hashset![],
                child("coll:dyn2") => hashset![],
            },
            process_component_dependencies(&instance)
        );

        let fidl_decl = decl.into();
        let offer_1 = fdecl::Offer::Protocol(fdecl::OfferProtocol {
            source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "childA".into(),
                collection: None,
            })),
            target: Some(fdecl::Ref::Child(fdecl::ChildRef {
                name: "dyn1".into(),
                collection: Some("coll".parse().unwrap()),
            })),
            target_name: Some("test.protocol".to_string()),
            dependency_type: Some(fdecl::DependencyType::Strong),
            ..Default::default()
        });

        let dynamic_offers = vec![offer_1];
        let sorted_map =
            process_deps(&fidl_decl, &vec![("dyn1", "coll"), ("dyn2", "coll")], &dynamic_offers)
                .topological_sort()
                .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("dyn1", Some("coll")),
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Child("dyn2", Some("coll")),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[test_case(DependencyType::Weak)]
    fn test_single_weak_dependency(weak_dep: DependencyType) {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("serviceSelf")
                    .source(OfferSource::Self_)
                    .target_static_child("childA")
                    .dependency(weak_dep.clone()),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childBOffer")
                    .target_name("serviceSibling")
                    .source_static_child("childB")
                    .target_static_child("childA")
                    .dependency(weak_dep.clone()),
            )
            .child_default("childA")
            .child_default("childB")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB")],
                child("childA") => hashset![],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();

        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_multiple_dependencies_same_source() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("serviceSelf")
                    .source(OfferSource::Self_)
                    .target_static_child("childA"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childBOffer")
                    .target_name("serviceSibling")
                    .source_static_child("childB")
                    .target_static_child("childA"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childBOtherOffer")
                    .target_name("serviceOtherSibling")
                    .source_static_child("childB")
                    .target_static_child("childA"),
            )
            .child_default("childA")
            .child_default("childB")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB")],
                child("childA") => hashset![],
                child("childB") => hashset![child("childA")],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();

        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_multiple_dependents_same_source() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("childBOffer")
                    .target_name("serviceSibling")
                    .source_static_child("childB")
                    .target_static_child("childA"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childBToC")
                    .target_name("serviceSibling")
                    .source_static_child("childB")
                    .target_static_child("childC"),
            )
            .child_default("childA")
            .child_default("childB")
            .child_default("childC")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("childB"),
                    child("childC"),
                ],
                child("childA") => hashset![],
                child("childB") => hashset![child("childA"), child("childC")],
                child("childC") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();

        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childC", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[test_case(DependencyType::Weak)]
    fn test_multiple_dependencies(weak_dep: DependencyType) {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("childBOffer")
                    .target_name("serviceSibling")
                    .source_static_child("childA")
                    .target_static_child("childC"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childBToC")
                    .target_name("serviceSibling")
                    .source_static_child("childB")
                    .target_static_child("childC"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childCToA")
                    .target_name("serviceSibling")
                    .source_static_child("childC")
                    .target_static_child("childA")
                    .dependency(weak_dep),
            )
            .child_default("childA")
            .child_default("childB")
            .child_default("childC")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("childB"),
                    child("childC"),
                ],
                child("childA") => hashset![child("childC")],
                child("childB") => hashset![child("childC")],
                child("childC") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();

        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childC", None),
            DependencyNode::Child("childA", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_component_is_source_and_target() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("childBOffer")
                    .target_name("serviceSibling")
                    .source_static_child("childA")
                    .target_static_child("childB"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childBToC")
                    .target_name("serviceSibling")
                    .source_static_child("childB")
                    .target_static_child("childC"),
            )
            .child_default("childA")
            .child_default("childB")
            .child_default("childC")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    child("childA"),
                    child("childB"),
                    child("childC"),
                ],
                child("childA") => hashset![child("childB")],
                child("childB") => hashset![child("childC")],
                child("childC") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childC", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    /// Tests a graph that looks like the below, tildes indicate a
    /// capability route. Route point toward the target of the capability
    /// offer. The manifest constructed is for 'P'.
    ///       P
    ///    ___|___
    ///  /  / | \  \
    /// e<~c<~a~>b~>d
    ///     \      /
    ///      *>~~>*
    #[fuchsia::test]
    fn test_complex_routing() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("childAService")
                    .source_static_child("childA")
                    .target_static_child("childB"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childAService")
                    .source_static_child("childA")
                    .target_static_child("childC"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childBService")
                    .source_static_child("childB")
                    .target_static_child("childD"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childAService")
                    .source_static_child("childC")
                    .target_static_child("childD"),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("childAService")
                    .source_static_child("childC")
                    .target_static_child("childE"),
            )
            .child_default("childA")
            .child_default("childB")
            .child_default("childC")
            .child_default("childD")
            .child_default("childE")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                   child("childA"),
                   child("childB"),
                   child("childC"),
                   child("childD"),
                   child("childE"),
                ],
                child("childA") => hashset![child("childB"), child("childC")],
                child("childB") => hashset![child("childD")],
                child("childC") => hashset![child("childD"), child("childE")],
                child("childD") => hashset![],
                child("childE") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];
        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childD", None),
            DependencyNode::Child("childB", None),
            DependencyNode::Child("childE", None),
            DependencyNode::Child("childC", None),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_service_from_collection() {
        let decl = ComponentDeclBuilder::new()
            .collection_default("coll")
            .child_default("static_child")
            .offer(
                OfferBuilder::service()
                    .name("service_capability")
                    .source(OfferSource::Collection("coll".parse().unwrap()))
                    .target_static_child("static_child"),
            )
            .build();

        let dynamic_child = ChildName::try_new("dynamic_child", Some("coll")).unwrap();
        let mut fake = FakeComponent::from_decl(decl.clone());
        fake.dynamic_children
            .push(Child { moniker: dynamic_child.clone(), environment_name: None });

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    ComponentRef::Child(dynamic_child.clone()),child("static_child")
                ],
                ComponentRef::Child(dynamic_child) => hashset![child("static_child")],
                child("static_child") => hashset![],
            },
            process_component_dependencies(&fake)
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map =
            process_deps(&fidl_decl, &vec![("dynamic_child", "coll")], &dynamic_offers)
                .topological_sort()
                .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("static_child", None),
            DependencyNode::Collection("coll"),
            DependencyNode::Child("dynamic_child", Some("coll")),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_use_service_from_collection_with_multiple_instances() {
        let decl = ComponentDeclBuilder::new()
            .collection_default("coll")
            .use_(
                UseBuilder::service()
                    .name("service_capability")
                    .source(UseSource::Collection("coll".parse().unwrap())),
            )
            .build();

        let dynamic_child1 = ChildName::try_new("dynamic_child1", Some("coll")).unwrap();
        let dynamic_child2 = ChildName::try_new("dynamic_child2", Some("coll")).unwrap();
        let mut fake = FakeComponent::from_decl(decl.clone());
        fake.dynamic_children
            .push(Child { moniker: dynamic_child1.clone(), environment_name: None });
        fake.dynamic_children
            .push(Child { moniker: dynamic_child2.clone(), environment_name: None });

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![],
                ComponentRef::Child(dynamic_child1) => hashset![ComponentRef::Self_],
                ComponentRef::Child(dynamic_child2) => hashset![ComponentRef::Self_],
            },
            process_component_dependencies(&fake)
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map = process_deps(
            &fidl_decl,
            &vec![("dynamic_child1", "coll"), ("dynamic_child2", "coll")],
            &dynamic_offers,
        )
        .topological_sort()
        .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Self_,
            DependencyNode::Child("dynamic_child1", Some("coll")),
            DependencyNode::Child("dynamic_child2", Some("coll")),
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_service_from_collection_with_multiple_instances() {
        let decl = ComponentDeclBuilder::new()
            .collection_default("coll")
            .child_default("static_child")
            .offer(
                OfferBuilder::service()
                    .name("service_capability")
                    .source(OfferSource::Collection("coll".parse().unwrap()))
                    .target_static_child("static_child"),
            )
            .build();

        let dynamic_child1 = ChildName::try_new("dynamic_child1", Some("coll")).unwrap();
        let dynamic_child2 = ChildName::try_new("dynamic_child2", Some("coll")).unwrap();
        let mut fake = FakeComponent::from_decl(decl.clone());
        fake.dynamic_children
            .push(Child { moniker: dynamic_child1.clone(), environment_name: None });
        fake.dynamic_children
            .push(Child { moniker: dynamic_child2.clone(), environment_name: None });

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![
                    ComponentRef::Child(dynamic_child1.clone()),
                    ComponentRef::Child(dynamic_child2.clone()),
                    child("static_child")
                ],
                ComponentRef::Child(dynamic_child1) => hashset![child("static_child")],
                ComponentRef::Child(dynamic_child2) => hashset![child("static_child")],
                child("static_child") => hashset![],
            },
            process_component_dependencies(&fake)
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map = process_deps(
            &fidl_decl,
            &vec![("dynamic_child1", "coll"), ("dynamic_child2", "coll")],
            &dynamic_offers,
        )
        .topological_sort()
        .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("static_child", None),
            DependencyNode::Collection("coll"),
            DependencyNode::Child("dynamic_child1", Some("coll")),
            DependencyNode::Child("dynamic_child2", Some("coll")),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_service_dependency_between_collections() {
        let decl = ComponentDeclBuilder::new()
            .collection_default("coll1")
            .collection_default("coll2")
            .offer(
                OfferBuilder::service()
                    .name("fuchsia.service.FakeService")
                    .source(OfferSource::Collection("coll1".parse().unwrap()))
                    .target(OfferTarget::Collection("coll2".parse().unwrap())),
            )
            .build();

        let source_child1 = ChildName::try_new("source_child1", Some("coll1")).unwrap();
        let source_child2 = ChildName::try_new("source_child2", Some("coll1")).unwrap();
        let target_child1 = ChildName::try_new("target_child1", Some("coll2")).unwrap();
        let target_child2 = ChildName::try_new("target_child2", Some("coll2")).unwrap();

        let mut fake = FakeComponent::from_decl(decl.clone());
        fake.dynamic_children
            .push(Child { moniker: source_child1.clone(), environment_name: None });
        fake.dynamic_children
            .push(Child { moniker: source_child2.clone(), environment_name: None });
        fake.dynamic_children
            .push(Child { moniker: target_child1.clone(), environment_name: None });
        fake.dynamic_children
            .push(Child { moniker: target_child2.clone(), environment_name: None });

        pretty_assertions::assert_eq! {
            hashmap! {
                ComponentRef::Self_ => hashset![
                    ComponentRef::Child(source_child1.clone()),
                    ComponentRef::Child(source_child2.clone()),
                    ComponentRef::Child(target_child1.clone()),
                    ComponentRef::Child(target_child2.clone()),
                ],
                ComponentRef::Child(source_child1) => hashset! [
                    ComponentRef::Child(target_child1.clone()),
                    ComponentRef::Child(target_child2.clone()),
                ],
                ComponentRef::Child(source_child2) => hashset! [
                    ComponentRef::Child(target_child1.clone()),
                    ComponentRef::Child(target_child2.clone()),
                ],
                ComponentRef::Child(target_child1) => hashset![],
                ComponentRef::Child(target_child2) => hashset![],
            },
            process_component_dependencies(&fake)
        };

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map = process_deps(
            &fidl_decl,
            &vec![
                ("target_child1", "coll2"),
                ("source_child2", "coll1"),
                ("target_child2", "coll2"),
                ("source_child1", "coll1"),
            ],
            &dynamic_offers,
        )
        .topological_sort()
        .unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("target_child1", Some("coll2")),
            DependencyNode::Child("target_child2", Some("coll2")),
            DependencyNode::Collection("coll2"),
            DependencyNode::Collection("coll1"),
            DependencyNode::Child("source_child1", Some("coll1")),
            DependencyNode::Child("source_child2", Some("coll1")),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_use_from_child() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("serviceSelf")
                    .source(OfferSource::Self_)
                    .target_static_child("childA")
                    .dependency(DependencyType::Weak),
            )
            .child_default("childA")
            .use_(UseBuilder::protocol().name("test.protocol").source_static_child("childA"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![],
                child("childA") => hashset![ComponentRef::Self_],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> =
            vec![DependencyNode::Self_, DependencyNode::Child("childA", None)];
        assert_eq!(ans, sorted_map);
    }

    #[fuchsia::test]
    fn test_use_from_dictionary() {
        let decl = ComponentDeclBuilder::new()
            .dictionary_default("dict")
            .offer(
                OfferBuilder::protocol()
                    .name("weakService")
                    .source(OfferSource::Self_)
                    .target_static_child("childA")
                    .dependency(DependencyType::Weak),
            )
            .offer(
                OfferBuilder::protocol()
                    .name("serviceA")
                    .source_static_child("childA")
                    .target(OfferTarget::Capability("dict".parse().unwrap())),
            )
            .child_default("childA")
            .use_(
                UseBuilder::protocol()
                    .name("serviceA")
                    .source(UseSource::Self_)
                    .from_dictionary("dict"),
            )
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![],
                child("childA") => hashset![capability("dict")],
                capability("dict") => hashset![ComponentRef::Self_],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Capability("dict"),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
            DependencyNode::Capability("serviceA"),
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_use_runner_from_child() {
        let decl = ComponentDeclBuilder::new_empty_component()
            .child_default("childA")
            .use_(UseBuilder::runner().name("test.runner").source_static_child("childA"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![],
                child("childA") => hashset![ComponentRef::Self_],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> =
            vec![DependencyNode::Self_, DependencyNode::Child("childA", None)];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_use_from_some_children() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("serviceSelf")
                    .source(OfferSource::Self_)
                    .target_static_child("childA")
                    .dependency(DependencyType::Weak),
            )
            .child_default("childA")
            .child_default("childB")
            .use_(UseBuilder::protocol().name("test.protocol").source_static_child("childA"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                // childB is a dependent because we consider all children
                // dependent, unless the component uses something from the
                // child.
                ComponentRef::Self_ => hashset![child("childB")],
                child("childA") => hashset![ComponentRef::Self_],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
            DependencyNode::Child("childA", None),
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_use_from_child_offer_storage() {
        let decl = ComponentDeclBuilder::new()
            .capability(
                CapabilityBuilder::storage()
                    .name("cdata")
                    .source(StorageDirectorySource::Child("childB".into()))
                    .backing_dir("directory"),
            )
            .capability(
                CapabilityBuilder::storage()
                    .name("pdata")
                    .source(StorageDirectorySource::Parent)
                    .backing_dir("directory"),
            )
            .offer(
                OfferBuilder::storage()
                    .name("cdata")
                    .source(OfferSource::Self_)
                    .target_static_child("childA"),
            )
            .offer(
                OfferBuilder::storage()
                    .name("pdata")
                    .source(OfferSource::Self_)
                    .target_static_child("childA"),
            )
            .child_default("childA")
            .child_default("childB")
            .use_(UseBuilder::protocol().name("test.protocol").source_static_child("childA"))
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![],
                child("childA") => hashset![ComponentRef::Self_],
                child("childB") => hashset![capability("cdata")],
                capability("pdata") => hashset![child("childA")],
                capability("cdata") => hashset![child("childA")],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Self_,
            DependencyNode::Child("childA", None),
            DependencyNode::Capability("cdata"),
            DependencyNode::Child("childB", None),
            DependencyNode::Capability("pdata"),
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_use_from_child_weak() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("serviceSelf")
                    .source(OfferSource::Self_)
                    .target_static_child("childA"),
            )
            .child_default("childA")
            .use_(
                UseBuilder::protocol()
                    .name("test.protocol")
                    .source_static_child("childA")
                    .dependency(DependencyType::Weak),
            )
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA")],
                child("childA") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> =
            vec![DependencyNode::Child("childA", None), DependencyNode::Self_];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_use_from_some_children_weak() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::protocol()
                    .name("serviceSelf")
                    .source(OfferSource::Self_)
                    .target_static_child("childA")
                    .dependency(DependencyType::Weak),
            )
            .child_default("childA")
            .child_default("childB")
            .use_(UseBuilder::protocol().name("test.protocol").source_static_child("childA"))
            .use_(
                UseBuilder::protocol()
                    .name("test.protocol2")
                    .source_static_child("childB")
                    .dependency(DependencyType::Weak),
            )
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                // childB is a dependent because its use-from-child has a 'weak' dependency.
                ComponentRef::Self_ => hashset![child("childB")],
                child("childA") => hashset![ComponentRef::Self_],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childB", None),
            DependencyNode::Self_,
            DependencyNode::Child("childA", None),
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    fn test_resolver_capability_creates_dependency() {
        let decl = ComponentDeclBuilder::new()
            .offer(
                OfferBuilder::resolver()
                    .name("resolver")
                    .source_static_child("childA")
                    .target_static_child("childB"),
            )
            .child_default("childA")
            .child_default("childB")
            .build();

        pretty_assertions::assert_eq!(
            hashmap! {
                ComponentRef::Self_ => hashset![child("childA"), child("childB")],
                child("childA") => hashset![child("childB")],
                child("childB") => hashset![],
            },
            process_component_dependencies(&FakeComponent::from_decl(decl.clone()))
        );

        let fidl_decl = decl.into();
        let dynamic_offers = vec![];

        let sorted_map =
            process_deps(&fidl_decl, &vec![], &dynamic_offers).topological_sort().unwrap();
        let ans: Vec<DependencyNode<'_>> = vec![
            DependencyNode::Child("childB", None),
            DependencyNode::Child("childA", None),
            DependencyNode::Self_,
        ];
        assert_eq!(ans, sorted_map)
    }

    #[fuchsia::test]
    async fn action_shutdown_blocks_stop() {
        let test = ActionsTest::new("root", vec![], None).await;
        let component = test.model.root().clone();

        let (mock_shutdown_barrier, mock_shutdown_action) = MockAction::new(ActionKey::Shutdown);

        // Register some actions, and get notifications.
        let shutdown_notifier = component.actions().register_no_wait(mock_shutdown_action).await;
        let stop_notifier = component.actions().register_no_wait(StopAction::new(false)).await;

        // The stop action should be blocked on the shutdown action completing.
        assert!(stop_notifier.fut.peek().is_none());

        // allow the shutdown action to finish running
        mock_shutdown_barrier.send(Ok(())).unwrap();
        shutdown_notifier.await.expect("shutdown failed unexpectedly");

        // The stop action should now finish running.
        stop_notifier.await.expect("stop failed unexpectedly");
    }

    #[fuchsia::test]
    async fn action_shutdown_stop_stop() {
        let test = ActionsTest::new("root", vec![], None).await;
        let component = test.model.root().clone();
        let (mock_shutdown_barrier, mock_shutdown_action) = MockAction::new(ActionKey::Shutdown);

        // Register some actions, and get notifications.
        let shutdown_notifier = component.actions().register_no_wait(mock_shutdown_action).await;
        let stop_notifier_1 = component.actions().register_no_wait(StopAction::new(false)).await;
        let stop_notifier_2 = component.actions().register_no_wait(StopAction::new(false)).await;

        // The stop action should be blocked on the shutdown action completing.
        assert!(stop_notifier_1.fut.peek().is_none());
        assert!(stop_notifier_2.fut.peek().is_none());

        // allow the shutdown action to finish running
        mock_shutdown_barrier.send(Ok(())).unwrap();
        shutdown_notifier.await.expect("shutdown failed unexpectedly");

        // The stop actions should now finish running.
        stop_notifier_1.await.expect("stop failed unexpectedly");
        stop_notifier_2.await.expect("stop failed unexpectedly");
    }

    #[fuchsia::test]
    async fn shutdown_one_component() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();

        // Start the component. This should cause the component to have an `Execution`.
        let component = test.look_up(["a"].try_into().unwrap()).await;
        root.start_instance(&component.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(component.is_started().await);
        let a_info = ComponentInfo::new(component.clone()).await;

        // Register shutdown action, and wait for it. Component should shut down (no more
        // `Execution`).
        ActionsManager::register(
            a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        a_info.check_is_shut_down(&test.runner).await;

        // Trying to start the component should fail because it's shut down.
        root.start_instance(&a_info.component.moniker, &StartReason::Eager)
            .await
            .expect_err("successfully bound to a after shutdown");

        // Shut down the component again. This succeeds, but has no additional effect.
        ActionsManager::register(
            a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        a_info.check_is_shut_down(&test.runner).await;
    }

    #[fuchsia::test]
    async fn shutdown_collection() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("container").build()),
            (
                "container",
                ComponentDeclBuilder::new().collection_default("coll").child_default("c").build(),
            ),
            ("a", component_decl_with_test_runner()),
            ("b", component_decl_with_test_runner()),
            ("c", component_decl_with_test_runner()),
        ];
        let test =
            ActionsTest::new("root", components, Some(["container"].try_into().unwrap())).await;
        let root = test.model.root();

        // Create dynamic instances in "coll".
        test.create_dynamic_child("coll", "a").await;
        test.create_dynamic_child("coll", "b").await;

        // Start the components. This should cause them to have an `Execution`.
        let component_container = test.look_up(["container"].try_into().unwrap()).await;
        let component_a = test.look_up(["container", "coll:a"].try_into().unwrap()).await;
        let component_b = test.look_up(["container", "coll:b"].try_into().unwrap()).await;
        let component_c = test.look_up(["container", "c"].try_into().unwrap()).await;
        root.start_instance(&component_container.moniker, &StartReason::Eager)
            .await
            .expect("could not start container");
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start coll:a");
        root.start_instance(&component_b.moniker, &StartReason::Eager)
            .await
            .expect("could not start coll:b");
        root.start_instance(&component_c.moniker, &StartReason::Eager)
            .await
            .expect("could not start c");
        assert!(component_container.is_started().await);
        assert!(component_a.is_started().await);
        assert!(component_b.is_started().await);
        assert!(component_c.is_started().await);
        assert!(has_child(&component_container, "coll:a").await);
        assert!(has_child(&component_container, "coll:b").await);

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_container_info = ComponentInfo::new(component_container).await;

        // Register shutdown action, and wait for it. Components should shut down (no more
        // `Execution`). Also, the instances in the collection should have been destroyed because
        // they were transient.
        ActionsManager::register(
            component_container_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_container_info.check_is_shut_down(&test.runner).await;
        assert!(!has_child(&component_container_info.component, "coll:a").await);
        assert!(!has_child(&component_container_info.component, "coll:b").await);
        assert!(has_child(&component_container_info.component, "c").await);
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;

        // Verify events.
        {
            let mut events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            // The leaves could be stopped in any order.
            let mut next: Vec<_> = events.drain(0..3).collect();
            next.sort_unstable();
            let expected: Vec<_> = vec![
                Lifecycle::Stop(["container", "c"].try_into().unwrap()),
                Lifecycle::Stop(["container", "coll:a"].try_into().unwrap()),
                Lifecycle::Stop(["container", "coll:b"].try_into().unwrap()),
            ];
            assert_eq!(next, expected);

            // These components were destroyed because they lived in a transient collection.
            let mut next: Vec<_> = events.drain(0..2).collect();
            next.sort_unstable();
            let expected: Vec<_> = vec![
                Lifecycle::Destroy(["container", "coll:a"].try_into().unwrap()),
                Lifecycle::Destroy(["container", "coll:b"].try_into().unwrap()),
            ];
            assert_eq!(next, expected);
        }
    }

    #[fuchsia::test]
    async fn shutdown_dynamic_offers() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("container").build()),
            (
                "container",
                ComponentDeclBuilder::new()
                    .collection(
                        CollectionBuilder::new()
                            .name("coll")
                            .allowed_offers(AllowedOffers::StaticAndDynamic),
                    )
                    .child_default("c")
                    .offer(
                        OfferBuilder::protocol()
                            .name("static_offer_source")
                            .target_name("static_offer_target")
                            .source(OfferSource::Child(ChildRef {
                                name: "c".parse().unwrap(),
                                collection: None,
                            }))
                            .target(OfferTarget::Collection("coll".parse().unwrap())),
                    )
                    .build(),
            ),
            ("a", component_decl_with_test_runner()),
            ("b", component_decl_with_test_runner()),
            ("c", component_decl_with_test_runner()),
        ];
        let test =
            ActionsTest::new("root", components, Some(["container"].try_into().unwrap())).await;
        let root = test.model.root();

        // Create dynamic instances in "coll".
        test.create_dynamic_child("coll", "a").await;
        test.create_dynamic_child_with_args(
            "coll",
            "b",
            fcomponent::CreateChildArgs {
                dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                    source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                        name: "a".into(),
                        collection: Some("coll".parse().unwrap()),
                    })),
                    source_name: Some("dyn_offer_source_name".to_string()),
                    target_name: Some("dyn_offer_target_name".to_string()),
                    dependency_type: Some(fdecl::DependencyType::Strong),
                    ..Default::default()
                })]),
                ..Default::default()
            },
        )
        .await
        .expect("failed to create child");

        // Start the components. This should cause them to have an `Execution`.
        let component_container = test.look_up(["container"].try_into().unwrap()).await;
        let component_a = test.look_up(["container", "coll:a"].try_into().unwrap()).await;
        let component_b = test.look_up(["container", "coll:b"].try_into().unwrap()).await;
        let component_c = test.look_up(["container", "c"].try_into().unwrap()).await;
        root.start_instance(&component_container.moniker, &StartReason::Eager)
            .await
            .expect("could not start container");
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start coll:a");
        root.start_instance(&component_b.moniker, &StartReason::Eager)
            .await
            .expect("could not start coll:b");
        root.start_instance(&component_c.moniker, &StartReason::Eager)
            .await
            .expect("could not start c");
        assert!(component_container.is_started().await);
        assert!(component_a.is_started().await);
        assert!(component_b.is_started().await);
        assert!(component_c.is_started().await);
        assert!(has_child(&component_container, "coll:a").await);
        assert!(has_child(&component_container, "coll:b").await);

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_container_info = ComponentInfo::new(component_container).await;

        // Register shutdown action, and wait for it. Components should shut down (no more
        // `Execution`). Also, the instances in the collection should have been destroyed because
        // they were transient.
        ActionsManager::register(
            component_container_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_container_info.check_is_shut_down(&test.runner).await;
        assert!(!has_child(&component_container_info.component, "coll:a").await);
        assert!(!has_child(&component_container_info.component, "coll:b").await);
        assert!(has_child(&component_container_info.component, "c").await);
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;

        // Verify events.
        {
            let mut events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();

            pretty_assertions::assert_eq!(
                vec![
                    Lifecycle::Stop(["container", "coll:b"].try_into().unwrap()),
                    Lifecycle::Stop(["container", "coll:a"].try_into().unwrap()),
                    Lifecycle::Stop(["container", "c"].try_into().unwrap()),
                ],
                events.drain(0..3).collect::<Vec<_>>()
            );

            // The order here is nondeterministic.
            pretty_assertions::assert_eq!(
                btreeset![
                    Lifecycle::Destroy(["container", "coll:b"].try_into().unwrap()),
                    Lifecycle::Destroy(["container", "coll:a"].try_into().unwrap()),
                ],
                events.drain(0..2).collect::<BTreeSet<_>>()
            );
            pretty_assertions::assert_eq!(
                vec![Lifecycle::Stop(["container"].try_into().unwrap()),],
                events
            );
        }
    }

    #[fuchsia::test]
    async fn shutdown_not_started() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            ("b", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        assert!(!component_a.is_started().await);
        assert!(!component_b.is_started().await);

        // Register shutdown action on "a", and wait for it.
        ActionsManager::register(component_a.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .expect("shutdown failed");
        assert!(execution_is_shut_down(&component_a).await);
        assert!(execution_is_shut_down(&component_b).await);

        // Now "a" is shut down. There should be no events though because the component was
        // never started.
        ActionsManager::register(component_a.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .expect("shutdown failed");
        assert!(execution_is_shut_down(&component_a).await);
        assert!(execution_is_shut_down(&component_b).await);
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(events, Vec::<Lifecycle>::new());
        }
    }

    #[fuchsia::test]
    async fn shutdown_not_resolved() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            ("b", ComponentDeclBuilder::new().child_default("c").build()),
            ("c", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(component_a.is_started().await);

        // Register shutdown action on "a", and wait for it.
        ActionsManager::register(component_a.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .expect("shutdown failed");
        assert!(execution_is_shut_down(&component_a).await);
        // Get component without resolving it.
        let component_b = {
            let state = component_a.lock_state().await;
            match *state {
                InstanceState::Shutdown(ref state, _) => {
                    state.children.get("b").expect("child b not found").clone()
                }
                _ => panic!("not shutdown"),
            }
        };
        assert!(execution_is_shut_down(&component_b).await);

        // Now "a" is shut down. There should be no event for "b" because it was never started
        // (or resolved).
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(events, vec![Lifecycle::Stop(["a"].try_into().unwrap())]);
        }
    }

    /// Shut down `a`:
    ///  a
    ///   \
    ///    b
    ///   / \
    ///  c   d
    #[fuchsia::test]
    async fn shutdown_hierarchy() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child(ChildBuilder::new().name("b").eager()).build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager())
                    .child(ChildBuilder::new().name("d").eager())
                    .build(),
            ),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(["a", "b", "d"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(component_a.is_started().await);
        assert!(component_b.is_started().await);
        assert!(component_c.is_started().await);
        assert!(component_d.is_started().await);

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;
        let component_d_info = ComponentInfo::new(component_d).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down, in bottom-up order.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;
        component_d_info.check_is_shut_down(&test.runner).await;
        {
            let mut events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            let mut first: Vec<_> = events.drain(0..2).collect();
            first.sort_unstable();
            let expected: Vec<_> = vec![
                Lifecycle::Stop(["a", "b", "c"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b", "d"].try_into().unwrap()),
            ];
            assert_eq!(first, expected);
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(["a", "b"].try_into().unwrap()),
                    Lifecycle::Stop(["a"].try_into().unwrap())
                ]
            );
        }
    }

    /// Shut down `a`:
    ///   a
    ///    \
    ///     b
    ///   / | \
    ///  c<-d->e
    /// In this case C and E use a service provided by d
    #[fuchsia::test]
    async fn shutdown_with_multiple_deps() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child(ChildBuilder::new().name("b").eager()).build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager())
                    .child(ChildBuilder::new().name("d").eager())
                    .child(ChildBuilder::new().name("e").eager())
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceD")
                            .source_static_child("d")
                            .target_static_child("c"),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceD")
                            .source_static_child("d")
                            .target_static_child("e"),
                    )
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("serviceD")).build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceD")
                    .expose(ExposeBuilder::protocol().name("serviceD").source(ExposeSource::Self_))
                    .build(),
            ),
            (
                "e",
                ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("serviceD")).build(),
            ),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(["a", "b", "d"].try_into().unwrap()).await;
        let component_e = test.look_up(["a", "b", "e"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(component_a.is_started().await);
        assert!(component_b.is_started().await);
        assert!(component_c.is_started().await);
        assert!(component_d.is_started().await);
        assert!(component_e.is_started().await);

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;
        let component_d_info = ComponentInfo::new(component_d).await;
        let component_e_info = ComponentInfo::new(component_e).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down, in bottom-up order.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;
        component_d_info.check_is_shut_down(&test.runner).await;
        component_e_info.check_is_shut_down(&test.runner).await;

        {
            let mut events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            let mut first: Vec<_> = events.drain(0..2).collect();
            first.sort_unstable();
            let mut expected: Vec<_> = vec![
                Lifecycle::Stop(["a", "b", "c"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b", "e"].try_into().unwrap()),
            ];
            assert_eq!(first, expected);

            let next: Vec<_> = events.drain(0..1).collect();
            expected = vec![Lifecycle::Stop(["a", "b", "d"].try_into().unwrap())];
            assert_eq!(next, expected);

            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(["a", "b"].try_into().unwrap()),
                    Lifecycle::Stop(["a"].try_into().unwrap())
                ]
            );
        }
    }

    /// Shut down `a`:
    ///    a
    ///     \
    ///      b
    ///   / / \  \
    ///  c<-d->e->f
    /// In this case C and E use a service provided by D and
    /// F uses a service provided by E, shutdown order should be
    /// {F}, {C, E}, {D}, {B}, {A}
    /// Note that C must stop before D, but may stop before or after
    /// either of F and E.
    #[fuchsia::test]
    async fn shutdown_with_multiple_out_and_longer_chain() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child(ChildBuilder::new().name("b").eager()).build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager())
                    .child(ChildBuilder::new().name("d").eager())
                    .child(ChildBuilder::new().name("e").eager())
                    .child(ChildBuilder::new().name("f").eager())
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceD")
                            .source_static_child("d")
                            .target_static_child("c"),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceD")
                            .source_static_child("d")
                            .target_static_child("e"),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceE")
                            .source_static_child("e")
                            .target_static_child("f"),
                    )
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("serviceD")).build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceD")
                    .expose(ExposeBuilder::protocol().name("serviceD").source(ExposeSource::Self_))
                    .build(),
            ),
            (
                "e",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceE")
                    .use_(UseBuilder::protocol().name("serviceD"))
                    .expose(ExposeBuilder::protocol().name("serviceE").source(ExposeSource::Self_))
                    .build(),
            ),
            (
                "f",
                ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("serviceE")).build(),
            ),
        ];
        let moniker_a: Moniker = ["a"].try_into().unwrap();
        let moniker_b: Moniker = ["a", "b"].try_into().unwrap();
        let moniker_c: Moniker = ["a", "b", "c"].try_into().unwrap();
        let moniker_d: Moniker = ["a", "b", "d"].try_into().unwrap();
        let moniker_e: Moniker = ["a", "b", "e"].try_into().unwrap();
        let moniker_f: Moniker = ["a", "b", "f"].try_into().unwrap();
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(moniker_a.clone()).await;
        let component_b = test.look_up(moniker_b.clone()).await;
        let component_c = test.look_up(moniker_c.clone()).await;
        let component_d = test.look_up(moniker_d.clone()).await;
        let component_e = test.look_up(moniker_e.clone()).await;
        let component_f = test.look_up(moniker_f.clone()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(component_a.is_started().await);
        assert!(component_b.is_started().await);
        assert!(component_c.is_started().await);
        assert!(component_d.is_started().await);
        assert!(component_e.is_started().await);
        assert!(component_f.is_started().await);

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;
        let component_d_info = ComponentInfo::new(component_d).await;
        let component_e_info = ComponentInfo::new(component_e).await;
        let component_f_info = ComponentInfo::new(component_f).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down, in bottom-up order.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;
        component_d_info.check_is_shut_down(&test.runner).await;
        component_e_info.check_is_shut_down(&test.runner).await;
        component_f_info.check_is_shut_down(&test.runner).await;

        let mut comes_after: HashMap<Moniker, Vec<Moniker>> = HashMap::new();
        comes_after.insert(moniker_a.clone(), vec![moniker_b.clone()]);
        // technically we could just depend on 'D' since it is the last of b's
        // children, but we add all the children for resilence against the
        // future
        comes_after.insert(
            moniker_b.clone(),
            vec![moniker_c.clone(), moniker_d.clone(), moniker_e.clone(), moniker_f.clone()],
        );
        comes_after.insert(moniker_d.clone(), vec![moniker_c.clone(), moniker_e.clone()]);
        comes_after.insert(moniker_c.clone(), vec![]);
        comes_after.insert(moniker_e.clone(), vec![moniker_f.clone()]);
        comes_after.insert(moniker_f.clone(), vec![]);
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();

            for e in events {
                match e {
                    Lifecycle::Stop(moniker) => match comes_after.remove(&moniker) {
                        Some(dependents) => {
                            for d in dependents {
                                if comes_after.contains_key(&d) {
                                    panic!("{} stopped before its dependent {}", moniker, d);
                                }
                            }
                        }
                        None => {
                            panic!("{} was unknown or shut down more than once", moniker);
                        }
                    },
                    _ => {
                        panic!("Unexpected lifecycle type");
                    }
                }
            }
        }
    }

    /// Shut down `a`:
    ///           a
    ///
    ///           |
    ///
    ///     +---- b ----+
    ///    /             \
    ///   /     /   \     \
    ///
    ///  c <~~ d ~~> e ~~> f
    ///          \       /
    ///           +~~>~~+
    /// In this case C and E use a service provided by D and
    /// F uses a services provided by E and D, shutdown order should be F must
    /// stop before E and {C,E,F} must stop before D. C may stop before or
    /// after either of {F, E}.
    #[fuchsia::test]
    async fn shutdown_with_multiple_out_multiple_in() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child(ChildBuilder::new().name("b").eager()).build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager())
                    .child(ChildBuilder::new().name("d").eager())
                    .child(ChildBuilder::new().name("e").eager())
                    .child(ChildBuilder::new().name("f").eager())
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceD")
                            .source_static_child("d")
                            .target_static_child("c"),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceD")
                            .source_static_child("d")
                            .target_static_child("e"),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceD")
                            .source_static_child("d")
                            .target_static_child("f"),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceE")
                            .source_static_child("e")
                            .target_static_child("f"),
                    )
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("serviceD")).build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceD")
                    .expose(ExposeBuilder::protocol().name("serviceD").source(ExposeSource::Self_))
                    .build(),
            ),
            (
                "e",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceE")
                    .use_(UseBuilder::protocol().name("serviceE"))
                    .expose(ExposeBuilder::protocol().name("serviceE").source(ExposeSource::Self_))
                    .build(),
            ),
            (
                "f",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::protocol().name("serviceE"))
                    .use_(UseBuilder::protocol().name("serviceD"))
                    .build(),
            ),
        ];
        let moniker_a: Moniker = ["a"].try_into().unwrap();
        let moniker_b: Moniker = ["a", "b"].try_into().unwrap();
        let moniker_c: Moniker = ["a", "b", "c"].try_into().unwrap();
        let moniker_d: Moniker = ["a", "b", "d"].try_into().unwrap();
        let moniker_e: Moniker = ["a", "b", "e"].try_into().unwrap();
        let moniker_f: Moniker = ["a", "b", "f"].try_into().unwrap();
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(moniker_a.clone()).await;
        let component_b = test.look_up(moniker_b.clone()).await;
        let component_c = test.look_up(moniker_c.clone()).await;
        let component_d = test.look_up(moniker_d.clone()).await;
        let component_e = test.look_up(moniker_e.clone()).await;
        let component_f = test.look_up(moniker_f.clone()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(component_a.is_started().await);
        assert!(component_b.is_started().await);
        assert!(component_c.is_started().await);
        assert!(component_d.is_started().await);
        assert!(component_e.is_started().await);
        assert!(component_f.is_started().await);

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;
        let component_d_info = ComponentInfo::new(component_d).await;
        let component_e_info = ComponentInfo::new(component_e).await;
        let component_f_info = ComponentInfo::new(component_f).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down, in bottom-up order.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;
        component_d_info.check_is_shut_down(&test.runner).await;
        component_e_info.check_is_shut_down(&test.runner).await;
        component_f_info.check_is_shut_down(&test.runner).await;

        let mut comes_after: HashMap<Moniker, Vec<Moniker>> = HashMap::new();
        comes_after.insert(moniker_a.clone(), vec![moniker_b.clone()]);
        // technically we could just depend on 'D' since it is the last of b's
        // children, but we add all the children for resilence against the
        // future
        comes_after.insert(
            moniker_b.clone(),
            vec![moniker_c.clone(), moniker_d.clone(), moniker_e.clone(), moniker_f.clone()],
        );
        comes_after.insert(
            moniker_d.clone(),
            vec![moniker_c.clone(), moniker_e.clone(), moniker_f.clone()],
        );
        comes_after.insert(moniker_c.clone(), vec![]);
        comes_after.insert(moniker_e.clone(), vec![moniker_f.clone()]);
        comes_after.insert(moniker_f.clone(), vec![]);
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();

            for e in events {
                match e {
                    Lifecycle::Stop(moniker) => {
                        let dependents = comes_after.remove(&moniker).unwrap_or_else(|| {
                            panic!("{} was unknown or shut down more than once", moniker)
                        });
                        for d in dependents {
                            if comes_after.contains_key(&d) {
                                panic!("{} stopped before its dependent {}", moniker, d);
                            }
                        }
                    }
                    _ => {
                        panic!("Unexpected lifecycle type");
                    }
                }
            }
        }
    }

    /// Shut down `a`:
    ///  a
    ///   \
    ///    b
    ///   / \
    ///  c-->d
    /// In this case D uses a resource exposed by C
    #[fuchsia::test]
    async fn shutdown_with_dependency() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child(ChildBuilder::new().name("b").eager()).build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager())
                    .child(ChildBuilder::new().name("d").eager())
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceC")
                            .source_static_child("c")
                            .target_static_child("d"),
                    )
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceC")
                    .expose(ExposeBuilder::protocol().name("serviceC").source(ExposeSource::Self_))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("serviceC")).build(),
            ),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(["a", "b", "d"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;
        let component_d_info = ComponentInfo::new(component_d).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down, in bottom-up and dependency order.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;
        component_d_info.check_is_shut_down(&test.runner).await;

        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            let expected: Vec<_> = vec![
                Lifecycle::Stop(["a", "b", "d"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b", "c"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b"].try_into().unwrap()),
                Lifecycle::Stop(["a"].try_into().unwrap()),
            ];
            assert_eq!(events, expected);
        }
    }

    /// Shut down `a`:
    ///  a
    ///   \
    ///    b
    ///   / \
    ///  c-->d
    /// In this case D uses a resource exposed by C, routed through a dictionary defined by B
    #[fuchsia::test]
    async fn shutdown_with_dictionary_dependency() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child(ChildBuilder::new().name("b").eager()).build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .dictionary_default("dict")
                    .child(ChildBuilder::new().name("c").eager())
                    .child(ChildBuilder::new().name("d").eager())
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceC")
                            .source_static_child("c")
                            .target(OfferTarget::Capability("dict".parse().unwrap())),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceC")
                            .source(OfferSource::Self_)
                            .from_dictionary("dict")
                            .target_static_child("d"),
                    )
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceC")
                    .expose(ExposeBuilder::protocol().name("serviceC").source(ExposeSource::Self_))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("serviceC")).build(),
            ),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(["a", "b", "d"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;
        let component_d_info = ComponentInfo::new(component_d).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down, in bottom-up and dependency order.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;
        component_d_info.check_is_shut_down(&test.runner).await;

        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            let expected: Vec<_> = vec![
                Lifecycle::Stop(["a", "b", "d"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b", "c"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b"].try_into().unwrap()),
                Lifecycle::Stop(["a"].try_into().unwrap()),
            ];
            assert_eq!(events, expected);
        }
    }

    /// Shut down `a`:
    ///   a     (a use b)
    ///  / \
    /// b    c
    /// In this case, c shuts down first, then a, then b.
    #[fuchsia::test]
    async fn shutdown_use_from_child() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager())
                    .child(ChildBuilder::new().name("c").eager())
                    .use_(UseBuilder::protocol().source_static_child("b").name("serviceC"))
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceC")
                    .expose(ExposeBuilder::protocol().name("serviceC").source(ExposeSource::Self_))
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new().build()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(["a", "c"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;

        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            let expected: Vec<_> = vec![
                Lifecycle::Stop(["a", "c"].try_into().unwrap()),
                Lifecycle::Stop(["a"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b"].try_into().unwrap()),
            ];
            assert_eq!(events, expected);
        }
    }

    /// Shut down `a`:
    ///   a     (a uses runner from b)
    ///  / \
    /// b    c
    /// In this case, c shuts down first, then a, then b.
    #[fuchsia::test]
    async fn test_shutdown_use_runner_from_child() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new_empty_component()
                    .child(ChildBuilder::new().name("b").eager())
                    .child(ChildBuilder::new().name("c").eager())
                    .use_(UseBuilder::runner().source_static_child("b").name("test.runner"))
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::runner().name("test.runner").source(ExposeSource::Self_))
                    .capability(CapabilityBuilder::runner().name("test.runner"))
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new().build()),
        ];
        let test = ActionsTest::new("root", components, None).await;

        // Set up component `b`'s outgoing directory to provide the test runner protocol, so that
        // component `a` can also be started. Without this the outgoing directory for `b` will be
        // closed by the mock runner and start requests for `a` will thus be ignored.
        //
        // We need a weak runner because otherwise the runner will hold the outgoing directory's
        // host function, which will hold a reference to the runner, creating a cycle.
        let weak_runner = Arc::downgrade(&test.runner);
        let mut test_runner_out_dir = OutDir::new();
        test_runner_out_dir.add_entry(
            "/svc/fuchsia.component.runner.ComponentRunner".parse().unwrap(),
            vfs::service::endpoint(move |_scope, channel| {
                fuchsia_async::Task::spawn(
                    weak_runner.upgrade().unwrap().handle_stream(
                        fcrunner::ComponentRunnerRequestStream::from_channel(channel),
                    ),
                )
                .detach();
            }),
        );
        test.runner.add_host_fn("test:///b", test_runner_out_dir.host_fn());

        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(["a", "c"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;

        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            let expected: Vec<_> = vec![
                Lifecycle::Stop(["a", "c"].try_into().unwrap()),
                Lifecycle::Stop(["a"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b"].try_into().unwrap()),
            ];
            assert_eq!(events, expected);
        }
    }

    /// Shut down `a`:
    ///   a     (a use b, and b use c)
    ///  / \
    /// b    c
    /// In this case, a shuts down first, then b, then c.
    #[fuchsia::test]
    async fn shutdown_use_from_child_that_uses_from_sibling() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager())
                    .child(ChildBuilder::new().name("c").eager())
                    .use_(UseBuilder::protocol().source_static_child("b").name("serviceB"))
                    .offer(
                        OfferBuilder::protocol()
                            .name("serviceC")
                            .target_name("serviceB")
                            .source_static_child("c")
                            .target_static_child("b"),
                    )
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceB")
                    .expose(ExposeBuilder::protocol().name("serviceB").source(ExposeSource::Self_))
                    .use_(UseBuilder::protocol().name("serviceC"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceC")
                    .expose(ExposeBuilder::protocol().name("serviceC").source(ExposeSource::Self_))
                    .build(),
            ),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(["a", "c"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;

        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            let expected: Vec<_> = vec![
                Lifecycle::Stop(["a"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b"].try_into().unwrap()),
                Lifecycle::Stop(["a", "c"].try_into().unwrap()),
            ];
            assert_eq!(events, expected);
        }
    }

    /// Shut down `a`:
    ///   a     (a use b weak)
    ///  / \
    /// b    c
    /// In this case, b or c shutdown first (arbitrary order), then a.
    #[fuchsia::test]
    async fn shutdown_use_from_child_weak() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager())
                    .child(ChildBuilder::new().name("c").eager())
                    .use_(
                        UseBuilder::protocol()
                            .source_static_child("b")
                            .name("serviceC")
                            .dependency(DependencyType::Weak),
                    )
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .protocol_default("serviceC")
                    .expose(ExposeBuilder::protocol().name("serviceC").source(ExposeSource::Self_))
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new().build()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(["a", "c"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_c_info = ComponentInfo::new(component_c).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;

        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            let expected1: Vec<_> = vec![
                Lifecycle::Stop(["a", "c"].try_into().unwrap()),
                Lifecycle::Stop(["a", "b"].try_into().unwrap()),
                Lifecycle::Stop(["a"].try_into().unwrap()),
            ];
            let expected2: Vec<_> = vec![
                Lifecycle::Stop(["a", "b"].try_into().unwrap()),
                Lifecycle::Stop(["a", "c"].try_into().unwrap()),
                Lifecycle::Stop(["a"].try_into().unwrap()),
            ];
            assert!(events == expected1 || events == expected2);
        }
    }

    /// Shut down `b`:
    ///  a
    ///   \
    ///    b
    ///     \
    ///      b
    ///       \
    ///      ...
    ///
    /// `b` is a child of itself, but shutdown should still be able to complete.
    #[fuchsia::test]
    async fn shutdown_self_referential() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            ("b", ComponentDeclBuilder::new().child_default("b").build()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();
        let component_a = test.look_up(["a"].try_into().unwrap()).await;
        let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
        let component_b2 = test.look_up(["a", "b", "b"].try_into().unwrap()).await;

        // Start second `b`.
        root.start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start b2");
        root.start_instance(&component_b.moniker, &StartReason::Eager)
            .await
            .expect("could not start b2");
        root.start_instance(&component_b2.moniker, &StartReason::Eager)
            .await
            .expect("could not start b2");
        assert!(component_a.is_started().await);
        assert!(component_b.is_started().await);
        assert!(component_b2.is_started().await);

        let component_a_info = ComponentInfo::new(component_a).await;
        let component_b_info = ComponentInfo::new(component_b).await;
        let component_b2_info = ComponentInfo::new(component_b2).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down, in bottom-up and dependency order.
        ActionsManager::register(
            component_a_info.component.clone(),
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("shutdown failed");
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_b2_info.check_is_shut_down(&test.runner).await;
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(["a", "b", "b"].try_into().unwrap()),
                    Lifecycle::Stop(["a", "b"].try_into().unwrap()),
                    Lifecycle::Stop(["a"].try_into().unwrap())
                ]
            );
        }
    }

    /// Shut down `a`:
    ///  a
    ///   \
    ///    b
    ///   / \
    ///  c   d
    ///
    /// `a` fails to finish shutdown the first time, but succeeds the second time.
    #[fuchsia::test]
    fn shutdown_error() {
        let mut executor = fasync::TestExecutor::new();
        let mut test_body = Box::pin(async move {
            let components = vec![
                ("root", ComponentDeclBuilder::new().child_default("a").build()),
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .child(ChildBuilder::new().name("b").eager())
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .child(ChildBuilder::new().name("c").eager())
                        .child(ChildBuilder::new().name("d").eager())
                        .build(),
                ),
                ("c", component_decl_with_test_runner()),
                ("d", component_decl_with_test_runner()),
            ];
            let test = ActionsTest::new("root", components, None).await;
            let root = test.model.root();
            let component_a = test.look_up(["a"].try_into().unwrap()).await;
            let component_b = test.look_up(["a", "b"].try_into().unwrap()).await;
            let component_c = test.look_up(["a", "b", "c"].try_into().unwrap()).await;
            let component_d = test.look_up(["a", "b", "d"].try_into().unwrap()).await;

            // Component startup was eager, so they should all have an `Execution`.
            root.start_instance(&component_a.moniker, &StartReason::Eager)
                .await
                .expect("could not start a");
            assert!(component_a.is_started().await);
            assert!(component_b.is_started().await);
            assert!(component_c.is_started().await);
            assert!(component_d.is_started().await);

            let component_a_info = ComponentInfo::new(component_a.clone()).await;
            let component_b_info = ComponentInfo::new(component_b).await;
            let component_c_info = ComponentInfo::new(component_c).await;
            let component_d_info = ComponentInfo::new(component_d.clone()).await;

            // Mock a failure to shutdown "d".
            let (shutdown_completer, mock_shutdown_action) = MockAction::new(ActionKey::Shutdown);
            let _shutdown_notifier =
                component_d.actions().register_no_wait(mock_shutdown_action).await;

            // Register shutdown action on "a", and wait for it. "d" fails to shutdown, so "a"
            // fails too. The state of "c" is unknown at this point. The shutdown of stop targets
            // occur simultaneously. "c" could've shutdown before "d" or it might not have.
            let a_shutdown_notifier = component_a
                .actions()
                .register_no_wait(ShutdownAction::new(ShutdownType::Instance))
                .await;

            // We need to wait for the shutdown action of "b" to register a shutdown action on "d",
            // which will be deduplicated with the shutdown action we registered on "d" earlier.
            _ = fasync::TestExecutor::poll_until_stalled(std::future::pending::<()>()).await;

            // Now we can allow the mock shutdown action to complete with an error, and wait for
            // our destroy child call to finish.
            shutdown_completer
                .send(Err(ActionError::StopError { err: StopActionError::GetParentFailed }))
                .unwrap();
            a_shutdown_notifier.await.expect_err("shutdown succeeded unexpectedly");

            component_a_info.check_not_shut_down(&test.runner).await;
            component_b_info.check_not_shut_down(&test.runner).await;
            component_d_info.check_not_shut_down(&test.runner).await;

            // Register shutdown action on "a" again. Without our mock action queued up on it,
            // "d"'s shutdown succeeds, and "a" is shutdown this time.
            ActionsManager::register(
                component_a_info.component.clone(),
                ShutdownAction::new(ShutdownType::Instance),
            )
            .await
            .expect("shutdown failed");
            component_a_info.check_is_shut_down(&test.runner).await;
            component_b_info.check_is_shut_down(&test.runner).await;
            component_c_info.check_is_shut_down(&test.runner).await;
            component_d_info.check_is_shut_down(&test.runner).await;
            {
                let mut events: Vec<_> = test
                    .test_hook
                    .lifecycle()
                    .into_iter()
                    .filter(|e| match e {
                        Lifecycle::Stop(_) => true,
                        _ => false,
                    })
                    .collect();
                // The leaves could be stopped in any order.
                let mut first: Vec<_> = events.drain(0..2).collect();
                first.sort_unstable();
                let expected: Vec<_> = vec![
                    Lifecycle::Stop(["a", "b", "c"].try_into().unwrap()),
                    Lifecycle::Stop(["a", "b", "d"].try_into().unwrap()),
                ];
                assert_eq!(first, expected);
                assert_eq!(
                    events,
                    vec![
                        Lifecycle::Stop(["a", "b"].try_into().unwrap()),
                        Lifecycle::Stop(["a"].try_into().unwrap())
                    ]
                );
            }
        });
        executor.run_until_stalled(&mut test_body).unwrap();
    }
}
