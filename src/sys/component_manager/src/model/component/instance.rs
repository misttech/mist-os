// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        bedrock::program::{self as program, ComponentStopOutcome, Program, StopRequestSuccess},
        framework::controller,
        model::{
            actions::{
                resolve::sandbox_construction::{
                    self, build_component_sandbox, extend_dict_with_offers,
                },
                shutdown, ActionKey, DiscoverAction, StopAction,
            },
            component::{
                Component, ComponentInstance, Package, StartReason, WeakComponentInstance,
                WeakExtendedInstance,
            },
            context::ModelContext,
            environment::Environment,
            error::{
                ActionError, AddChildError, CreateNamespaceError, DynamicOfferError,
                OpenOutgoingDirError, ResolveActionError,
            },
            escrow::{self, EscrowedState},
            hooks::{CapabilityReceiver, Event, EventPayload},
            namespace::create_namespace,
            routing::{
                self,
                router::{Request, Routable, Router},
                service::{AnonymizedAggregateServiceDir, AnonymizedServiceRoute},
                RoutingError,
            },
            routing_fns::RouteEntry,
            start::Start,
            structured_dict::{ComponentEnvironment, ComponentInput, StructuredDictMap},
            token::{InstanceToken, InstanceTokenState},
        },
        sandbox_util::{DictExt, RoutableExt},
    },
    ::routing::{
        capability_source::ComponentCapability,
        component_instance::{
            ComponentInstanceInterface, ResolvedInstanceInterface, ResolvedInstanceInterfaceExt,
        },
        resolving::{ComponentAddress, ComponentResolutionContext},
    },
    async_trait::async_trait,
    async_utils::async_once::Once,
    clonable_error::ClonableError,
    cm_fidl_validator::error::DeclType,
    cm_logger::scoped::ScopedLogger,
    cm_moniker::{IncarnationId, InstancedChildName},
    cm_rust::{
        CapabilityDecl, CapabilityTypeName, ChildDecl, CollectionDecl, ComponentDecl, DeliveryType,
        FidlIntoNative, NativeIntoFidl, OfferDeclCommon, SourceName, UseDecl,
    },
    cm_types::Name,
    config_encoder::ConfigFields,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_io as fio, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::future::{BoxFuture, FutureExt},
    moniker::{ChildName, ChildNameBase, Moniker, MonikerBase},
    sandbox::{Capability, CapabilityTrait, Dict, Open},
    std::{
        collections::{HashMap, HashSet},
        fmt,
        sync::Arc,
    },
    tracing::warn,
    vfs::{directory::immutable::simple as pfs, path::Path},
};

/// The mutable state of a component instance.
pub enum InstanceState {
    /// The instance was just created.
    New,
    /// A Discovered event has been dispatched for the instance, but it has not been resolved yet.
    Unresolved(UnresolvedInstanceState),
    /// The instance has been resolved.
    Resolved(ResolvedInstanceState),
    /// The instance has started running.
    Started(ResolvedInstanceState, StartedInstanceState),
    /// The instance has been shutdown, and may not run anymore.
    Shutdown(ShutdownInstanceState, UnresolvedInstanceState),
    /// The instance has been destroyed. It has no content and no further actions may be registered
    /// on it.
    Destroyed,
}

impl InstanceState {
    pub fn replace<F>(&mut self, f: F)
    where
        F: FnOnce(InstanceState) -> InstanceState,
    {
        // We place InstanceState::New into self temporarily, so that the function can take
        // ownership of the current InstanceState and move values out of it.
        *self = f(std::mem::replace(self, InstanceState::New));
    }

    /// Changes the state, checking invariants.
    /// The allowed transitions:
    /// • New -> Discovered <-> Resolved -> Destroyed
    /// • {New, Discovered, Resolved} -> Destroyed
    pub fn set(&mut self, next: Self) {
        match (&self, &next) {
            (Self::New, Self::New)
            | (Self::New, Self::Resolved(_))
            | (Self::Unresolved(_), Self::Unresolved(_))
            | (Self::Unresolved(_), Self::New)
            | (Self::Resolved(_), Self::Resolved(_))
            | (Self::Resolved(_), Self::New)
            | (Self::Destroyed, Self::Destroyed)
            | (Self::Destroyed, Self::New)
            | (Self::Destroyed, Self::Unresolved(_))
            | (Self::Destroyed, Self::Resolved(_)) => {
                panic!("Invalid instance state transition from {:?} to {:?}", self, next);
            }
            _ => {
                *self = next;
            }
        }
    }

    pub fn is_shut_down(&self) -> bool {
        match &self {
            InstanceState::Shutdown(_, _) | InstanceState::Destroyed => true,
            _ => false,
        }
    }

    pub fn is_started(&self) -> bool {
        self.get_started_state().is_some()
    }

    pub fn get_resolved_state(&self) -> Option<&ResolvedInstanceState> {
        match &self {
            InstanceState::Resolved(state) | InstanceState::Started(state, _) => Some(state),
            _ => None,
        }
    }

    pub fn get_resolved_state_mut(&mut self) -> Option<&mut ResolvedInstanceState> {
        match self {
            InstanceState::Resolved(ref mut state) | InstanceState::Started(ref mut state, _) => {
                Some(state)
            }
            _ => None,
        }
    }

    pub fn get_started_state(&self) -> Option<&StartedInstanceState> {
        match &self {
            InstanceState::Started(_, state) => Some(state),
            _ => None,
        }
    }

    pub fn get_started_state_mut(&mut self) -> Option<&mut StartedInstanceState> {
        match self {
            InstanceState::Started(_, ref mut state) => Some(state),
            _ => None,
        }
    }

    /// Requests a token that represents this component instance, minting it if needed.
    ///
    /// If the component instance is destroyed or not discovered, returns `None`.
    pub fn instance_token(
        &mut self,
        moniker: &Moniker,
        context: &Arc<ModelContext>,
    ) -> Option<InstanceToken> {
        match self {
            InstanceState::New => None,
            InstanceState::Unresolved(unresolved_state)
            | InstanceState::Shutdown(_, unresolved_state) => {
                Some(unresolved_state.instance_token(moniker, context))
            }
            InstanceState::Resolved(resolved) | InstanceState::Started(resolved, _) => {
                Some(resolved.instance_token(moniker, context))
            }
            InstanceState::Destroyed => None,
        }
    }

    /// Removes any escrowed state such that they can be passed back to the component
    /// as it is started.
    pub async fn reap_escrowed_state_during_start(&mut self) -> Option<EscrowedState> {
        let escrow = self.get_resolved_state().and_then(|state| state.program_escrow())?;
        escrow.will_start().await
    }

    /// Scope server_end to `StartedInstanceState`. This ensures that the channel will be kept
    /// alive as long as the component is running. If the component is not started when this method
    /// is called, this operation is a no-op and the channel will be dropped.
    pub fn scope_server_end(&mut self, server_end: zx::Channel) {
        if let Some(started_state) = self.get_started_state_mut() {
            started_state.add_scoped_server_end(server_end);
        }
    }
}

impl fmt::Debug for InstanceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::New => "New",
            Self::Unresolved(_) => "Discovered",
            Self::Resolved(_) => "Resolved",
            Self::Started(_, _) => "Started",
            Self::Shutdown(_, _) => "Shutdown",
            Self::Destroyed => "Destroyed",
        };
        f.write_str(s)
    }
}

pub struct ShutdownInstanceState {
    /// The children of this component, which is retained in case a destroy action is performed, as
    /// in that case the children will need to be destroyed as well.
    pub children: HashMap<ChildName, Arc<ComponentInstance>>,

    /// Information about used storage capabilities the component had in its manifest. This is
    /// retained because the storage contents will be deleted if this component is destroyed.
    pub routed_storage: Vec<routing::RoutedStorage>,
}

pub struct UnresolvedInstanceState {
    /// Caches an instance token.
    instance_token_state: InstanceTokenState,

    /// The dict containing all capabilities that the parent wished to provide to us.
    pub component_input: ComponentInput,
}

impl UnresolvedInstanceState {
    pub fn new(component_input: ComponentInput) -> Self {
        Self { instance_token_state: Default::default(), component_input }
    }

    fn instance_token(&mut self, moniker: &Moniker, context: &Arc<ModelContext>) -> InstanceToken {
        self.instance_token_state.set(moniker, context)
    }

    /// Returns relevant information and prepares to enter the resolved state.
    pub fn to_resolved(&mut self) -> (InstanceTokenState, ComponentInput) {
        (std::mem::take(&mut self.instance_token_state), self.component_input.clone())
    }

    /// Creates a new UnresolvedInstanceState by either cloning values from this struct or moving
    /// values from it (and replacing the values with their default values). This struct should be
    /// dropped after this function is called.
    pub fn take(&mut self) -> Self {
        Self {
            instance_token_state: std::mem::take(&mut self.instance_token_state),
            component_input: self.component_input.clone(),
        }
    }
}

/// Expose instance state in the format in which the `shutdown` action expects
/// to see it.
///
/// Largely shares its implementation with `ResolvedInstanceInterface`.
impl shutdown::Component for ResolvedInstanceState {
    fn uses(&self) -> Vec<UseDecl> {
        <Self as ResolvedInstanceInterface>::uses(self)
    }

    fn exposes(&self) -> Vec<cm_rust::ExposeDecl> {
        <Self as ResolvedInstanceInterface>::exposes(self)
    }

    fn offers(&self) -> Vec<cm_rust::OfferDecl> {
        // Includes both static and dynamic offers.
        <Self as ResolvedInstanceInterface>::offers(self)
    }

    fn capabilities(&self) -> Vec<cm_rust::CapabilityDecl> {
        <Self as ResolvedInstanceInterface>::capabilities(self)
    }

    fn collections(&self) -> Vec<cm_rust::CollectionDecl> {
        <Self as ResolvedInstanceInterface>::collections(self)
    }

    fn environments(&self) -> Vec<cm_rust::EnvironmentDecl> {
        self.resolved_component.decl.environments.clone()
    }

    fn children(&self) -> Vec<shutdown::Child> {
        // Includes both static and dynamic children.
        ResolvedInstanceState::children(self)
            .map(|(moniker, instance)| shutdown::Child {
                moniker: moniker.clone(),
                environment_name: instance.environment().name().map(|n| n.to_string()),
            })
            .collect()
    }
}

/// The mutable state of a resolved component instance.
pub struct ResolvedInstanceState {
    /// Weak reference to the component that owns this state.
    weak_component: WeakComponentInstance,

    /// Caches an instance token.
    instance_token_state: InstanceTokenState,

    /// Result of resolving the component.
    pub resolved_component: Component,

    /// All child instances, indexed by child moniker.
    pub children: HashMap<ChildName, Arc<ComponentInstance>>,

    /// The next unique identifier for a dynamic children created in this realm.
    /// (Static instances receive identifier 0.)
    next_dynamic_instance_id: IncarnationId,

    /// The set of named Environments defined by this instance.
    environments: HashMap<String, Arc<Environment>>,

    /// Directory that represents the program's namespace.
    ///
    /// This is only used for introspection, e.g. in RealmQuery. The program receives a
    /// namespace created in StartAction. The latter may have additional entries from
    /// [StartChildArgs].
    namespace_dir: Once<Arc<pfs::Simple>>,

    /// Hosts a directory mapping the component's exposed capabilities, generated from `exposed_dict`.
    /// Created on demand.
    exposed_dir: Once<Open>,

    /// Dynamic capabilities this component supports.
    ///
    /// For now, these are added in the the `AddChild` API for a realm, and we only
    /// support configuration capabilities. In the `AddChild` API these are paired with
    /// a dynamic offer, and are removed when that dynamic offer is removed.
    dynamic_capabilities: Vec<cm_rust::CapabilityDecl>,

    /// Dynamic offers targeting this component's dynamic children.
    ///
    /// Invariant: the `target` field of all offers must refer to a live dynamic
    /// child (i.e., a member of `live_children`), and if the `source` field
    /// refers to a dynamic child, it must also be live.
    dynamic_offers: Vec<cm_rust::OfferDecl>,

    /// The as-resolved location of the component: either an absolute component
    /// URL, or (with a package context) a relative path URL.
    address: ComponentAddress,

    /// Anonymized service directories aggregated from collections and children.
    pub anonymized_services: HashMap<AnonymizedServiceRoute, Arc<AnonymizedAggregateServiceDir>>,

    /// The dict containing all capabilities that the parent wished to provide to us.
    pub component_input: ComponentInput,

    /// The dict containing all capabilities that we expose.
    pub component_output_dict: Dict,

    /// The dict containing all capabilities that we use.
    pub program_input_dict: Dict,

    /// The dict containing all capabilities that we declare, as Routers.
    pub program_output_dict: Dict,

    /// Dictionary of extra capabilities passed to the component when it is started.
    // TODO(b/322564390): Move this into `StartedInstanceState` once stop action releases lock on it
    pub program_input_dict_additions: Option<Dict>,

    /// Dicts containing the capabilities we want to provide to each collection. Each new
    /// dynamic child gets a clone of one of these inputs (which is potentially extended by
    /// dynamic offers).
    pub collection_inputs: StructuredDictMap<ComponentInput>,

    /// The environments declared by this component.
    bedrock_environments: StructuredDictMap<ComponentEnvironment>,

    /// State held by the framework on behalf of the component's program, including
    /// its outgoing directory server endpoint. Present if and only if the component
    /// has a program.
    program_escrow: Option<escrow::Actor>,
}

/// Extracts a mutable reference to the `target` field of an `OfferDecl`, or
/// `None` if the offer type is unknown.
fn offer_target_mut(offer: &mut fdecl::Offer) -> Option<&mut Option<fdecl::Ref>> {
    match offer {
        fdecl::Offer::Service(fdecl::OfferService { target, .. })
        | fdecl::Offer::Protocol(fdecl::OfferProtocol { target, .. })
        | fdecl::Offer::Directory(fdecl::OfferDirectory { target, .. })
        | fdecl::Offer::Storage(fdecl::OfferStorage { target, .. })
        | fdecl::Offer::Runner(fdecl::OfferRunner { target, .. })
        | fdecl::Offer::Config(fdecl::OfferConfiguration { target, .. })
        | fdecl::Offer::Resolver(fdecl::OfferResolver { target, .. }) => Some(target),
        fdecl::OfferUnknown!() => None,
    }
}

impl ResolvedInstanceState {
    pub async fn new(
        component: &Arc<ComponentInstance>,
        resolved_component: Component,
        address: ComponentAddress,
        instance_token_state: InstanceTokenState,
        component_input: ComponentInput,
    ) -> Result<Self, ResolveActionError> {
        let weak_component = WeakComponentInstance::new(component);

        let environments = Self::instantiate_environments(component, &resolved_component.decl);
        let decl = resolved_component.decl.clone();

        // Perform the policy check for debug capabilities now, instead of during routing. All the info we
        // need to perform this check is already available to us. This way, we don't have to propagate this
        // info to sandbox capabilities just so they can enforce the policy.
        for (env_name, env) in &environments {
            for capability_name in env.environment().debug_registry().debug_capabilities.keys() {
                let parent = match env.environment().parent() {
                    WeakExtendedInstance::Component(p) => p,
                    WeakExtendedInstance::AboveRoot(_) => unreachable!(
                        "this environment was defined by the component, can't be \
                        from component_manager"
                    ),
                };
                component.policy_checker().can_register_debug_capability(
                    CapabilityTypeName::Protocol,
                    capability_name,
                    &parent.moniker,
                    &env_name,
                )?;
            }
        }

        let program_escrow = if decl.program.is_some() {
            let (escrow, escrow_task) = escrow::Actor::new(component.as_weak());
            component.nonblocking_task_group().spawn(escrow_task);
            Some(escrow)
        } else {
            None
        };

        let mut state = Self {
            weak_component,
            instance_token_state,
            resolved_component,
            children: HashMap::new(),
            next_dynamic_instance_id: 1,
            environments,
            namespace_dir: Once::default(),
            exposed_dir: Once::default(),
            dynamic_capabilities: vec![],
            dynamic_offers: vec![],
            address,
            anonymized_services: HashMap::new(),
            component_input,
            component_output_dict: Dict::new(),
            program_input_dict: Dict::new(),
            program_output_dict: Dict::new(),
            program_input_dict_additions: None,
            collection_inputs: Default::default(),
            bedrock_environments: Default::default(),
            program_escrow,
        };
        state.add_static_children(component).await?;

        let mut child_inputs = Default::default();
        build_component_sandbox(
            component,
            &state.children,
            &decl,
            &state.component_input,
            &state.component_output_dict,
            &state.program_input_dict,
            &state.program_output_dict,
            &mut child_inputs,
            &mut state.collection_inputs,
            &mut state.bedrock_environments,
        );
        state.discover_static_children(child_inputs).await;
        Ok(state)
    }

    /// Creates a `Router` that requests the specified capability from the
    /// program's outgoing directory.
    pub fn make_program_outgoing_router(
        component: &Arc<ComponentInstance>,
        component_decl: &ComponentDecl,
        capability_decl: &cm_rust::CapabilityDecl,
        path: &cm_types::Path,
    ) -> Router {
        if component_decl.program.is_none() {
            return Router::new_error(OpenOutgoingDirError::InstanceNonExecutable);
        }
        let path = path.to_string();
        let name = capability_decl.name();
        let path = fuchsia_fs::canonicalize_path(&path);
        let entry_type = ComponentCapability::from(capability_decl.clone()).type_name().into();
        let open = component
            .get_outgoing()
            .downscope_path(Path::validate_and_split(path).unwrap(), entry_type)
            .expect("get_outgoing must return a directory node");
        let capability: Capability = match capability_decl {
            CapabilityDecl::Protocol(_) => sandbox::Sender::new_sendable(open).into(),
            _ => open.into(),
        };
        let hook =
            CapabilityRequestedHook { source: component.as_weak(), name: name.clone(), capability };
        match capability_decl {
            CapabilityDecl::Protocol(p) => match p.delivery {
                DeliveryType::Immediate => Router::new(hook),
                DeliveryType::OnReadable => {
                    hook.on_readable(component.execution_scope.clone(), entry_type)
                }
            },
            _ => Router::new(hook),
        }
    }

    /// Returns a reference to the component's validated declaration.
    pub fn decl(&self) -> &ComponentDecl {
        &self.resolved_component.decl
    }

    #[cfg(test)]
    pub fn decl_as_mut(&mut self) -> &mut ComponentDecl {
        &mut self.resolved_component.decl
    }

    /// Returns relevant information and prepares to enter the unresolved state.
    pub fn to_unresolved(&mut self) -> UnresolvedInstanceState {
        UnresolvedInstanceState {
            instance_token_state: std::mem::replace(
                &mut self.instance_token_state,
                Default::default(),
            ),
            component_input: self.component_input.clone(),
        }
    }

    fn instance_token(&mut self, moniker: &Moniker, context: &Arc<ModelContext>) -> InstanceToken {
        self.instance_token_state.set(moniker, context)
    }

    pub fn program_escrow(&self) -> Option<&escrow::Actor> {
        self.program_escrow.as_ref()
    }

    pub fn address_for_relative_url(
        &self,
        fragment: &str,
    ) -> Result<ComponentAddress, ::routing::resolving::ResolverError> {
        self.address.clone_with_new_resource(fragment.strip_prefix("#"))
    }

    /// Returns an iterator over all children.
    pub fn children(&self) -> impl Iterator<Item = (&ChildName, &Arc<ComponentInstance>)> {
        self.children.iter().map(|(k, v)| (k, v))
    }

    /// Returns a reference to a child.
    pub fn get_child(&self, m: &ChildName) -> Option<&Arc<ComponentInstance>> {
        self.children.get(m)
    }

    /// Returns a vector of the children in `collection`.
    pub fn children_in_collection(
        &self,
        collection: &Name,
    ) -> Vec<(ChildName, Arc<ComponentInstance>)> {
        self.children()
            .filter(move |(m, _)| match m.collection() {
                Some(name) if name == collection => true,
                _ => false,
            })
            .map(|(m, c)| (m.clone(), Arc::clone(c)))
            .collect()
    }

    /// Returns a directory that represents the program's namespace at resolution time.
    ///
    /// This may not exactly match the namespace when the component is started since StartAction
    /// may add additional entries.
    pub async fn namespace_dir(&self) -> Result<Arc<pfs::Simple>, CreateNamespaceError> {
        let create_namespace_dir = async {
            let component = self
                .weak_component
                .upgrade()
                .map_err(CreateNamespaceError::ComponentInstanceError)?;
            // Build a namespace and convert it to a directory.
            let namespace_builder = create_namespace(
                self.resolved_component.package.as_ref(),
                &component,
                &self.resolved_component.decl,
                &self.program_input_dict,
                component.execution_scope.clone(),
            )
            .await?;
            let namespace =
                namespace_builder.serve().map_err(CreateNamespaceError::BuildNamespaceError)?;
            let namespace_dir: Arc<pfs::Simple> = namespace.try_into().map_err(|err| {
                CreateNamespaceError::ConvertToDirectory(ClonableError::from(anyhow::Error::from(
                    err,
                )))
            })?;
            Ok(namespace_dir)
        };

        Ok(self
            .namespace_dir
            .get_or_try_init::<_, CreateNamespaceError>(create_namespace_dir)
            .await?
            .clone())
    }

    /// Returns a [`Dict`] with contents similar to `component_output_dict`, but adds
    /// capabilities backed by legacy routing, and hosts [`Open`]s instead of
    /// [`Router`]s. This [`Dict`] is used to generate the `exposed_dir`. This function creates a new [`Dict`],
    /// so allocation cost is paid only when called.
    pub async fn make_exposed_dict(&self) -> Dict {
        let dict = Router::dict_routers_to_open(&self.weak_component, &self.component_output_dict);
        Self::extend_exposed_dict_with_legacy(&self.weak_component, self.decl(), &dict);
        dict
    }

    fn extend_exposed_dict_with_legacy(
        component: &WeakComponentInstance,
        decl: &cm_rust::ComponentDecl,
        target_dict: &Dict,
    ) {
        // Filter out capabilities handled by bedrock routing
        let exposes = decl.exposes.iter().filter(|e| !sandbox_construction::is_supported_expose(e));
        let exposes_by_target_name = routing::aggregate_exposes(exposes);
        for (target_name, exposes) in exposes_by_target_name {
            // If there are multiple exposes, choosing the first expose for `cap`. `cap` is only used
            // for debug info.
            //
            // TODO(https://fxbug.dev/42124541): This could lead to incomplete debug output because the source name
            // is what's printed, so if the exposes have different source names only one of them will
            // appear in the output. However, in practice routing is unlikely to fail for an aggregate
            // because the algorithm typically terminates once an aggregate is found. Find a more robust
            // solution, such as including all exposes or switching to the target name.
            let first_expose = *exposes.first().expect("empty exposes is impossible");
            let cap = ComponentCapability::Expose(first_expose.clone());
            let type_name = cap.type_name();
            let request = match routing::request_for_namespace_capability_expose(exposes) {
                Some(r) => r,
                None => continue,
            };
            let open = Open::new(RouteEntry::new(component.clone(), request, type_name.into()));
            target_dict.insert_capability(target_name, open.into());
        }
    }

    pub async fn get_exposed_dir(&self) -> &Open {
        let create_exposed_dir = async {
            let exposed_dict = self.make_exposed_dict().await;
            Open::new(
                exposed_dict
                    .try_into_directory_entry()
                    .expect("converting exposed dict to open should always succeed"),
            )
        };
        self.exposed_dir.get_or_init(create_exposed_dir).await
    }

    /// Returns the resolved structured configuration of this instance, if any.
    pub fn config(&self) -> Option<&ConfigFields> {
        self.resolved_component.config.as_ref()
    }

    /// Returns information about the package of the instance, if any.
    pub fn package(&self) -> Option<&Package> {
        self.resolved_component.package.as_ref()
    }

    /// Removes a child.
    pub fn remove_child(&mut self, moniker: &ChildName) {
        if self.children.remove(moniker).is_none() {
            return;
        }

        let mut capability_names_to_remove = HashSet::new();

        // Delete any dynamic offers whose `source` or `target` matches the
        // component we're deleting.
        self.dynamic_offers.retain(|offer| {
            let source_matches = offer.source()
                == &cm_rust::OfferSource::Child(cm_rust::ChildRef {
                    name: moniker.name().to_string().into(),
                    collection: moniker.collection().map(|c| c.clone()),
                });
            let target_matches = offer.target()
                == &cm_rust::OfferTarget::Child(cm_rust::ChildRef {
                    name: moniker.name().to_string().into(),
                    collection: moniker.collection().map(|c| c.clone()),
                });
            if target_matches && offer.source() == &cm_rust::OfferSource::Self_ {
                capability_names_to_remove.insert(offer.source_name().clone());
            }
            !source_matches && !target_matches
        });
        // Delete any dynamic capabilities whose `source` or `target` matches the
        // component we're deleting.
        self.dynamic_capabilities.retain(|cap| !capability_names_to_remove.contains(cap.name()))
    }

    /// Creates a set of Environments instantiated from their EnvironmentDecls.
    fn instantiate_environments(
        component: &Arc<ComponentInstance>,
        decl: &ComponentDecl,
    ) -> HashMap<String, Arc<Environment>> {
        let mut environments = HashMap::new();
        for env_decl in &decl.environments {
            environments.insert(
                env_decl.name.clone(),
                Arc::new(Environment::from_decl(component, env_decl)),
            );
        }
        environments
    }

    /// Retrieve an environment for `child`, inheriting from `component`'s environment if
    /// necessary.
    fn environment_for_child(
        &mut self,
        component: &Arc<ComponentInstance>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
    ) -> Arc<Environment> {
        // For instances in a collection, the environment (if any) is designated in the collection.
        // Otherwise, it's specified in the ChildDecl.
        let environment_name = match collection {
            Some(c) => c.environment.as_ref(),
            None => child.environment.as_ref(),
        };
        self.get_environment(component, environment_name)
    }

    fn get_environment(
        &self,
        component: &Arc<ComponentInstance>,
        environment_name: Option<&String>,
    ) -> Arc<Environment> {
        if let Some(environment_name) = environment_name {
            Arc::clone(
                self.environments
                    .get(environment_name)
                    .unwrap_or_else(|| panic!("Environment not found: {}", environment_name)),
            )
        } else {
            // Auto-inherit the environment from this component instance.
            Arc::new(Environment::new_inheriting(component))
        }
    }

    pub fn environment_for_collection(
        &self,
        component: &Arc<ComponentInstance>,
        collection: &CollectionDecl,
    ) -> Arc<Environment> {
        self.get_environment(component, collection.environment.as_ref())
    }

    /// Adds a new child component instance.
    ///
    /// The new child starts with a registered `Discover` action. Returns the child and a future to
    /// wait on the `Discover` action, or an error if a child with the same name already exists.
    ///
    /// If the outer `Result` is successful but the `Discover` future results in an error, the
    /// `Discover` action failed, but the child was still created successfully.
    pub async fn add_child(
        &mut self,
        component: &Arc<ComponentInstance>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
        dynamic_offers: Option<Vec<fdecl::Offer>>,
        dynamic_capabilities: Option<Vec<fdecl::Capability>>,
        controller: Option<ServerEnd<fcomponent::ControllerMarker>>,
        input: ComponentInput,
    ) -> Result<(Arc<ComponentInstance>, BoxFuture<'static, Result<(), ActionError>>), AddChildError>
    {
        let (child, input) = self
            .add_child_internal(
                component,
                child,
                collection,
                dynamic_offers,
                dynamic_capabilities,
                controller,
                input,
            )
            .await?;
        // Register a Discover action.
        let discover_fut = child
            .clone()
            .lock_actions()
            .await
            .register_no_wait(&child, DiscoverAction::new(input))
            .await
            .boxed();
        Ok((child, discover_fut))
    }

    /// Adds a new child of this instance for the given `ChildDecl`. Returns
    /// a result indicating if the new child instance has been successfully added.
    /// Like `add_child`, but doesn't register a `Discover` action, and therefore
    /// doesn't return a future to wait for.
    pub async fn add_child_no_discover(
        &mut self,
        component: &Arc<ComponentInstance>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
    ) -> Result<(), AddChildError> {
        self.add_child_internal(
            component,
            child,
            collection,
            None,
            None,
            None,
            ComponentInput::default(),
        )
        .await
        .map(|_| ())
    }

    async fn add_child_internal(
        &mut self,
        component: &Arc<ComponentInstance>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
        dynamic_offers: Option<Vec<fdecl::Offer>>,
        dynamic_capabilities: Option<Vec<fdecl::Capability>>,
        controller: Option<ServerEnd<fcomponent::ControllerMarker>>,
        mut child_input: ComponentInput,
    ) -> Result<(Arc<ComponentInstance>, ComponentInput), AddChildError> {
        assert!(
            (dynamic_offers.is_none()) || collection.is_some(),
            "setting numbered handles or dynamic offers for static children",
        );
        let (dynamic_offers, dynamic_capabilities) = self.validate_and_convert_dynamic_component(
            dynamic_offers,
            dynamic_capabilities,
            child,
            collection,
        )?;

        let child_moniker =
            ChildName::try_new(child.name.as_str(), collection.map(|c| c.name.as_str()))?;

        if !dynamic_offers.is_empty() {
            extend_dict_with_offers(
                component,
                &self.children,
                &self.component_input,
                &dynamic_offers,
                &mut child_input,
            );
        }

        if self.get_child(&child_moniker).is_some() {
            return Err(AddChildError::InstanceAlreadyExists {
                moniker: component.moniker().clone(),
                child: child_moniker,
            });
        }
        // TODO(https://fxbug.dev/42059793): next_dynamic_instance_id should be per-collection.
        let instance_id = match collection {
            Some(_) => {
                let id = self.next_dynamic_instance_id;
                self.next_dynamic_instance_id += 1;
                id
            }
            None => 0,
        };
        let instanced_moniker = InstancedChildName::from_child_moniker(&child_moniker, instance_id);
        let child = ComponentInstance::new(
            self.environment_for_child(component, child, collection.clone()),
            component.instanced_moniker.child(instanced_moniker),
            child.url.clone(),
            child.startup,
            child.on_terminate.unwrap_or(fdecl::OnTerminate::None),
            child.config_overrides.clone(),
            component.context.clone(),
            WeakExtendedInstance::Component(WeakComponentInstance::from(component)),
            component.hooks.clone(),
            component.persistent_storage_for_child(collection),
        )
        .await;
        if let Some(controller) = controller {
            if let Ok(stream) = controller.into_stream() {
                child
                    .nonblocking_task_group()
                    .spawn(controller::run_controller(WeakComponentInstance::new(&child), stream));
            }
        }
        self.children.insert(child_moniker, child.clone());

        self.dynamic_offers.extend(dynamic_offers.into_iter());
        self.dynamic_capabilities.extend(dynamic_capabilities.into_iter());

        Ok((child, child_input))
    }

    fn add_target_dynamic_offers(
        &self,
        mut dynamic_offers: Vec<fdecl::Offer>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
    ) -> Result<Vec<fdecl::Offer>, DynamicOfferError> {
        for offer in dynamic_offers.iter_mut() {
            match offer {
                fdecl::Offer::Service(fdecl::OfferService { target, .. })
                | fdecl::Offer::Protocol(fdecl::OfferProtocol { target, .. })
                | fdecl::Offer::Directory(fdecl::OfferDirectory { target, .. })
                | fdecl::Offer::Storage(fdecl::OfferStorage { target, .. })
                | fdecl::Offer::Runner(fdecl::OfferRunner { target, .. })
                | fdecl::Offer::Resolver(fdecl::OfferResolver { target, .. })
                | fdecl::Offer::Config(fdecl::OfferConfiguration { target, .. })
                | fdecl::Offer::EventStream(fdecl::OfferEventStream { target, .. }) => {
                    if target.is_some() {
                        return Err(DynamicOfferError::OfferInvalid {
                            err: cm_fidl_validator::error::ErrorList {
                                errs: vec![cm_fidl_validator::error::Error::extraneous_field(
                                    DeclType::Offer,
                                    "target",
                                )],
                            },
                        });
                    }
                }
                _ => {
                    return Err(DynamicOfferError::UnknownOfferType);
                }
            }
            *offer_target_mut(offer).expect("validation should have found unknown enum type") =
                Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: child.name.clone().into(),
                    collection: Some(collection.unwrap().name.clone().into()),
                }));
        }
        Ok(dynamic_offers)
    }

    fn validate_dynamic_component(
        &self,
        dynamic_offers: Vec<fdecl::Offer>,
        dynamic_capabilities: Vec<fdecl::Capability>,
    ) -> Result<(), AddChildError> {
        // Combine all our dynamic offers.
        let mut all_dynamic_offers: Vec<_> =
            self.dynamic_offers.clone().into_iter().map(NativeIntoFidl::native_into_fidl).collect();
        all_dynamic_offers.append(&mut dynamic_offers.clone());

        // Combine all our dynamic capabilities.
        let mut decl = self.resolved_component.decl.clone();
        decl.capabilities.extend(self.dynamic_capabilities.clone().into_iter());
        let mut decl = decl.native_into_fidl();
        match &mut decl.capabilities.as_mut() {
            Some(c) => c.extend(dynamic_capabilities.into_iter()),
            None => decl.capabilities = Some(dynamic_capabilities),
        }

        // Validate!
        cm_fidl_validator::validate_dynamic_offers(&all_dynamic_offers, &decl)?;

        // Manifest validation is not informed of the contents of collections, and is thus unable
        // to confirm the source exists if it's in a collection. Let's check that here.
        let dynamic_offers: Vec<cm_rust::OfferDecl> =
            dynamic_offers.into_iter().map(FidlIntoNative::fidl_into_native).collect();
        for offer in &dynamic_offers {
            if !self.offer_source_exists(offer.source()) {
                return Err(DynamicOfferError::SourceNotFound { offer: offer.clone() }.into());
            }
        }
        Ok(())
    }

    pub fn validate_and_convert_dynamic_component(
        &self,
        dynamic_offers: Option<Vec<fdecl::Offer>>,
        dynamic_capabilities: Option<Vec<fdecl::Capability>>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
    ) -> Result<(Vec<cm_rust::OfferDecl>, Vec<cm_rust::CapabilityDecl>), AddChildError> {
        let dynamic_offers = dynamic_offers.unwrap_or_default();
        let dynamic_capabilities = dynamic_capabilities.unwrap_or_default();

        let dynamic_offers = self.add_target_dynamic_offers(dynamic_offers, child, collection)?;
        if !dynamic_offers.is_empty() || !dynamic_capabilities.is_empty() {
            self.validate_dynamic_component(dynamic_offers.clone(), dynamic_capabilities.clone())?;
        }
        let dynamic_offers = dynamic_offers.into_iter().map(|o| o.fidl_into_native()).collect();
        let dynamic_capabilities =
            dynamic_capabilities.into_iter().map(|c| c.fidl_into_native()).collect();
        Ok((dynamic_offers, dynamic_capabilities))
    }

    async fn add_static_children(
        &mut self,
        component: &Arc<ComponentInstance>,
    ) -> Result<(), ResolveActionError> {
        // We can't hold an immutable reference to `self` while passing a mutable reference later
        // on. To get around this, clone the children.
        let children = self.resolved_component.decl.children.clone();
        for child in &children {
            self.add_child_no_discover(component, child, None).await.map_err(|err| {
                ResolveActionError::AddStaticChildError { child_name: child.name.to_string(), err }
            })?;
        }
        Ok(())
    }

    async fn discover_static_children(&self, mut child_inputs: StructuredDictMap<ComponentInput>) {
        for (child_name, child_instance) in &self.children {
            let child_name = Name::new(child_name.name()).unwrap();
            let child_input = child_inputs.remove(&child_name).expect("missing child dict");
            let _discover_fut = child_instance
                .clone()
                .lock_actions()
                .await
                .register_no_wait(&child_instance, DiscoverAction::new(child_input))
                .await;
        }
    }
}

impl ResolvedInstanceInterface for ResolvedInstanceState {
    type Component = ComponentInstance;

    fn uses(&self) -> Vec<UseDecl> {
        self.resolved_component.decl.uses.clone()
    }

    fn exposes(&self) -> Vec<cm_rust::ExposeDecl> {
        self.resolved_component.decl.exposes.clone()
    }

    fn offers(&self) -> Vec<cm_rust::OfferDecl> {
        self.resolved_component
            .decl
            .offers
            .iter()
            .chain(self.dynamic_offers.iter())
            .cloned()
            .collect()
    }

    fn capabilities(&self) -> Vec<cm_rust::CapabilityDecl> {
        self.resolved_component
            .decl
            .capabilities
            .iter()
            .chain(self.dynamic_capabilities.iter())
            .cloned()
            .collect()
    }

    fn collections(&self) -> Vec<cm_rust::CollectionDecl> {
        self.resolved_component.decl.collections.clone()
    }

    fn get_child(&self, moniker: &ChildName) -> Option<Arc<ComponentInstance>> {
        ResolvedInstanceState::get_child(self, moniker).map(Arc::clone)
    }

    fn children_in_collection(
        &self,
        collection: &Name,
    ) -> Vec<(ChildName, Arc<ComponentInstance>)> {
        ResolvedInstanceState::children_in_collection(self, collection)
    }

    fn address(&self) -> ComponentAddress {
        self.address.clone()
    }

    fn context_to_resolve_children(&self) -> Option<ComponentResolutionContext> {
        self.resolved_component.context_to_resolve_children.clone()
    }
}

/// The execution state for a program instance that is running.
struct ProgramRuntime {
    /// Used to interact with the Runner to influence the program's execution.
    program: Program,

    /// Listens for the controller channel to close in the background. This task is cancelled when
    /// the [`ProgramRuntime`] is dropped.
    exit_listener: fasync::Task<()>,
}

pub struct StopOutcomeWithEscrow {
    pub outcome: ComponentStopOutcome,
    pub escrow_request: Option<program::EscrowRequest>,
}

impl ProgramRuntime {
    pub fn new(program: Program, component: WeakComponentInstance) -> Self {
        let terminated_fut = program.on_terminate();
        let exit_listener = fasync::Task::spawn(async move {
            terminated_fut.await;
            if let Ok(component) = component.upgrade() {
                let mut actions = component.lock_actions().await;
                let stop_nf = actions.register_no_wait(&component, StopAction::new(false)).await;
                drop(actions);
                component.nonblocking_task_group().spawn(fasync::Task::spawn(async move {
                    let _ = stop_nf.await.map_err(
                        |err| warn!(%err, "Watching for program termination: Stop failed"),
                    );
                }));
            }
        });
        Self { program, exit_listener }
    }

    pub async fn stop<'a, 'b>(
        self,
        stop_timer: BoxFuture<'a, ()>,
        kill_timer: BoxFuture<'b, ()>,
    ) -> Result<StopOutcomeWithEscrow, program::StopError> {
        let outcome = self.program.stop_or_kill_with_timeout(stop_timer, kill_timer).await;
        // Drop the program and join on the exit listener. Dropping the program
        // should cause the exit listener to stop waiting for the channel epitaph and
        // exit.
        //
        // Note: this is more reliable than just cancelling `exit_listener` because
        // even after cancellation future may still run for a short period of time
        // before getting dropped. If that happens there is a chance of scheduling a
        // duplicate Stop action.
        let escrow_request = self.program.finalize();
        self.exit_listener.await;
        let outcome = outcome?;
        Ok(StopOutcomeWithEscrow { outcome, escrow_request })
    }
}

/// The execution state for a component instance that has started running.
///
/// If the component instance has a program, it may also have a [`ProgramRuntime`].
pub struct StartedInstanceState {
    /// If set, that means this component is associated with a running program.
    program: Option<ProgramRuntime>,

    /// Approximates when the component was started.
    pub timestamp: zx::Time,

    /// Describes why the component instance was started
    pub start_reason: StartReason,

    /// Channels scoped to lifetime of this component's execution context. This
    /// should only be used for the server_end of the `fuchsia.component.Binder`
    /// connection.
    binder_server_ends: Vec<zx::Channel>,

    /// This stores the hook for notifying an ExecutionController about stop events for this
    /// component.
    pub execution_controller_task: Option<controller::ExecutionControllerTask>,

    /// Logger attributed to this component.
    ///
    /// Only set if the component uses the `fuchsia.logger.LogSink` protocol.
    pub logger: Option<Arc<ScopedLogger>>,
}

impl StartedInstanceState {
    pub fn new(
        start_reason: StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        logger: Option<ScopedLogger>,
    ) -> Self {
        let timestamp = zx::Time::get_monotonic();
        StartedInstanceState {
            program: None,
            timestamp,
            binder_server_ends: vec![],
            start_reason,
            execution_controller_task,
            logger: logger.map(Arc::new),
        }
    }

    /// If this component is associated with a running [Program], obtain a capability
    /// representing its runtime directory.
    pub fn runtime_dir(&self) -> Option<&fio::DirectoryProxy> {
        self.program.as_ref().map(|program_runtime| program_runtime.program.runtime())
    }

    /// Associates the [StartedInstanceState] with a running [Program].
    ///
    /// Creates a background task waiting for the program to terminate. When that happens, use the
    /// [WeakComponentInstance] to stop the component.
    pub fn set_program(&mut self, program: Program, component: WeakComponentInstance) {
        self.program = Some(ProgramRuntime::new(program, component));
    }

    /// Stop the program, if any. The timer defines how long the runner is given to stop the
    /// program gracefully before we request the controller to terminate the program.
    ///
    /// Regardless if the runner honored our request, after this method, the [`StartedInstanceState`] is
    /// no longer associated with a [Program].
    pub async fn stop_program<'a, 'b>(
        &'a mut self,
        stop_timer: BoxFuture<'a, ()>,
        kill_timer: BoxFuture<'b, ()>,
    ) -> Result<StopOutcomeWithEscrow, program::StopError> {
        let program = self.program.take();
        // Potentially there is no program, perhaps because the component
        // has no running code. In this case this is a no-op.
        if let Some(program) = program {
            program.stop(stop_timer, kill_timer).await
        } else {
            Ok(StopOutcomeWithEscrow {
                outcome: ComponentStopOutcome {
                    request: StopRequestSuccess::NoController,
                    component_exit_status: zx::Status::OK,
                },
                escrow_request: None,
            })
        }
    }

    /// Add a channel scoped to the lifetime of this object.
    pub fn add_scoped_server_end(&mut self, server_end: zx::Channel) {
        self.binder_server_ends.push(server_end);
    }

    /// Gets a [`Koid`] that will uniquely identify the program.
    #[cfg(test)]
    pub fn program_koid(&self) -> Option<fuchsia_zircon::Koid> {
        self.program.as_ref().map(|program_runtime| program_runtime.program.koid())
    }
}

/// This delegates to the event system if the capability request is
/// intercepted by some hook, and delegates to the current capability otherwise.
#[derive(Debug)]
struct CapabilityRequestedHook {
    source: WeakComponentInstance,
    name: Name,
    capability: Capability,
}

#[async_trait]
impl Routable for CapabilityRequestedHook {
    async fn route(&self, request: Request) -> Result<Capability, bedrock_error::BedrockError> {
        self.source
            .ensure_started(&StartReason::AccessCapability {
                target: request.target.moniker.clone(),
                name: self.name.clone(),
            })
            .await?;
        let source = self.source.upgrade().map_err(RoutingError::from)?;
        let target = request.target.upgrade().map_err(RoutingError::from)?;
        let (receiver, sender) = CapabilityReceiver::new();
        let event = Event::new(
            &target,
            EventPayload::CapabilityRequested {
                source_moniker: source.moniker.clone(),
                name: self.name.to_string(),
                receiver: receiver.clone(),
            },
        );
        // TODO(https://fxbug.dev/320698181): Before dispatching events we need to wait for any
        // in-progress resolve actions to end. See https://fxbug.dev/320698181#comment21 for why.
        {
            let resolve_completed =
                source.lock_actions().await.wait_for_action(ActionKey::Resolve).await;
            resolve_completed.await.unwrap();
        }
        source.hooks.dispatch(&event).await;
        if receiver.is_taken() {
            Ok(sender.into())
        } else {
            Ok(self.capability.clone().into())
        }
    }
}
