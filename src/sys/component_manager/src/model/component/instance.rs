// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::program::{Program, StopConclusion, StopDisposition};
use crate::framework::{build_framework_dictionary, controller};
use crate::model::actions::{shutdown, StopAction};
use crate::model::component::{
    Component, ComponentInstance, ExtendedInstance, IncarnationId, Package, StartReason,
    WeakComponentInstance, WeakExtendedInstance,
};
use crate::model::context::ModelContext;
use crate::model::environment::Environment;
use crate::model::escrow::{self, EscrowedState};
use crate::model::namespace::create_namespace;
use crate::model::routing::legacy::RouteRequestExt;
use crate::model::routing::service::{AnonymizedAggregateServiceDir, AnonymizedServiceRoute};
use crate::model::routing::{self, RoutingFailureErrorReporter};
use crate::model::start::Start;
use crate::model::storage::build_storage_admin_dictionary;
use crate::model::token::{InstanceToken, InstanceTokenState};
use crate::sandbox_util::RoutableExt;
use ::routing::bedrock::program_output_dict::{
    build_program_output_dictionary, ProgramOutputGenerator,
};
use ::routing::bedrock::sandbox_construction::{
    self, build_component_sandbox, extend_dict_with_offers, ComponentSandbox,
};
use ::routing::bedrock::structured_dict::{ComponentInput, StructuredDictMap};
use ::routing::capability_source::{CapabilitySource, ComponentCapability, ComponentSource};
use ::routing::component_instance::{
    ComponentInstanceInterface, ResolvedInstanceInterface, ResolvedInstanceInterfaceExt,
    WeakComponentInstanceInterface,
};
use ::routing::error::{ComponentInstanceError, RoutingError};
use ::routing::resolving::{ComponentAddress, ComponentResolutionContext};
use ::routing::{DictExt, RouteRequest, WeakInstanceTokenExt};
use async_trait::async_trait;
use async_utils::async_once::Once;
use clonable_error::ClonableError;
use cm_fidl_validator::error::{DeclType, Error as ValidatorError};
use cm_logger::scoped::ScopedLogger;
use cm_rust::{
    CapabilityDecl, CapabilityTypeName, ChildDecl, CollectionDecl, ComponentDecl, DeliveryType,
    FidlIntoNative, NativeIntoFidl, OfferDeclCommon, UseDecl,
};
use cm_types::{Name, Path};
use config_encoder::ConfigFields;
use derivative::Derivative;
use errors::{
    AddChildError, AddDynamicChildError, CapabilityProviderError, ComponentProviderError,
    CreateNamespaceError, DynamicCapabilityError, OpenError, OpenOutgoingDirError,
    ResolveActionError, StopError,
};
use fidl::endpoints::{create_proxy, ServerEnd};
use futures::future::BoxFuture;
use hooks::{CapabilityReceiver, EventPayload};
use moniker::{ChildName, ExtendedMoniker, Moniker};
use router_error::RouterError;
use sandbox::{
    Capability, Connector, Dict, DirEntry, RemotableCapability, Request, Routable, Router,
    RouterResponse, WeakInstanceToken,
};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use tracing::warn;
use vfs::directory::entry::{DirectoryEntry, OpenRequest, SubNode};
use vfs::directory::immutable::simple as pfs;
use vfs::execution_scope::ExecutionScope;
use vfs::ToObjectRequest;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    zx,
};

/// The mutable state of a component instance.
pub enum InstanceState {
    /// The instance has not been resolved yet. This is the initial state.
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
        // We place InstanceState::Destroyed into self temporarily, so that the function can take
        // ownership of the current InstanceState and move values out of it.
        *self = f(std::mem::replace(self, InstanceState::Destroyed));
    }

    /// Changes the state, checking invariants.
    /// The allowed transitions:
    /// • Unresolved <-> Resolved -> Destroyed
    /// • {Unresolved, Resolved} -> Destroyed
    pub fn set(&mut self, next: Self) {
        match (&self, &next) {
            (Self::Unresolved(_), Self::Unresolved(_))
            | (Self::Resolved(_), Self::Resolved(_))
            | (Self::Destroyed, Self::Destroyed)
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
            Self::Unresolved(_) => "Unresolved",
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
                environment_name: instance.environment().name().cloned(),
            })
            .collect()
    }
}

/// The mutable state of a resolved component instance.
pub struct ResolvedInstanceState {
    /// Weak reference to the component that owns this state.
    weak_component: WeakComponentInstance,

    /// The component's execution scope, shared with [ComponentInstance::execution_scope].
    execution_scope: ExecutionScope,

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
    environments: HashMap<Name, Arc<Environment>>,

    /// Directory that represents the program's namespace.
    ///
    /// This is only used for introspection, e.g. in RealmQuery. The program receives a
    /// namespace created in StartAction. The latter may have additional entries from
    /// [StartChildArgs].
    namespace_dir: Once<Arc<pfs::Simple>>,

    /// Holds a [Dict] mapping the component's exposed capabilities. Created on demand.
    exposed_dict: Once<Dict>,

    /// Hosts a directory mapping the component's exposed capabilities, generated from `exposed_dict`.
    /// Created on demand.
    exposed_dir: Once<DirEntry>,

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

    /// The sandbox holds all dictionaries involved in capability routing.
    pub sandbox: ComponentSandbox,

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

        let program_escrow = if decl.get_runner().is_some() {
            let escrow =
                escrow::Actor::new(&component.nonblocking_task_group(), component.as_weak());
            Some(escrow)
        } else {
            None
        };

        let mut state = Self {
            weak_component,
            execution_scope: component.execution_scope.clone(),
            instance_token_state,
            resolved_component,
            children: HashMap::new(),
            next_dynamic_instance_id: 1,
            environments,
            namespace_dir: Once::default(),
            exposed_dict: Once::default(),
            exposed_dir: Once::default(),
            dynamic_offers: vec![],
            address,
            anonymized_services: HashMap::new(),
            sandbox: Default::default(),
            program_escrow,
        };
        state.add_static_children(component).await?;

        struct MyGenerator {}
        impl ProgramOutputGenerator<ComponentInstance> for MyGenerator {
            fn new_program_dictionary_router(
                &self,
                component: WeakComponentInstanceInterface<ComponentInstance>,
                source_path: Path,
                capability: ComponentCapability,
            ) -> Router<Dict> {
                Router::<Dict>::new(ProgramDictionaryRouter { component, source_path, capability })
            }

            fn new_outgoing_dir_connector_router(
                &self,
                component: &Arc<ComponentInstance>,
                decl: &cm_rust::ComponentDecl,
                capability: &cm_rust::CapabilityDecl,
            ) -> Router<Connector> {
                ResolvedInstanceState::make_program_outgoing_connector_router(
                    component, decl, capability,
                )
            }

            fn new_outgoing_dir_dir_entry_router(
                &self,
                component: &Arc<ComponentInstance>,
                decl: &cm_rust::ComponentDecl,
                capability: &cm_rust::CapabilityDecl,
            ) -> Router<DirEntry> {
                ResolvedInstanceState::make_program_outgoing_dir_entry_router(
                    component, decl, capability,
                )
            }
        }
        let child_outgoing_dictionary_routers =
            state.get_child_component_output_dictionary_routers();
        let (program_output_dict, declared_dictionaries) =
            build_program_output_dictionary(component, &decl, &MyGenerator {});

        let component_sandbox = build_component_sandbox(
            &component,
            child_outgoing_dictionary_routers,
            &decl,
            component_input,
            program_output_dict,
            build_framework_dictionary(component),
            build_storage_admin_dictionary(component, &decl),
            declared_dictionaries,
            RoutingFailureErrorReporter::new(component.as_weak()),
        );
        Self::extend_program_input_namespace_with_legacy(
            &component,
            &state.resolved_component.decl,
            &component_sandbox.program_input.namespace(),
        );

        state.sandbox = component_sandbox;
        state.populate_child_inputs(&state.sandbox.child_inputs).await;
        Ok(state)
    }

    /// Creates a `ConnectorRouter` that requests the specified capability from the
    /// program's outgoing directory.
    pub fn make_program_outgoing_connector_router(
        component: &Arc<ComponentInstance>,
        component_decl: &ComponentDecl,
        capability_decl: &cm_rust::CapabilityDecl,
    ) -> Router<Connector> {
        if component_decl.get_runner().is_none() {
            return Router::<Connector>::new_error(OpenOutgoingDirError::InstanceNonExecutable);
        }
        let name = capability_decl.name();
        let path = capability_decl.path().expect("must have path").to_string();
        let path = fuchsia_fs::canonicalize_path(&path);
        let entry_type = ComponentCapability::from(capability_decl.clone()).type_name().into();
        let relative_path = vfs::path::Path::validate_and_split(path).unwrap();
        let outgoing_dir_entry = component
            .get_outgoing()
            .try_into_directory_entry(component.execution_scope.clone())
            .expect("conversion to directory entry should succeed");
        #[derive(Derivative)]
        #[derivative(Debug)]
        struct OutgoingConnector {
            #[derivative(Debug = "ignore")]
            node: Arc<dyn DirectoryEntry>,
        }
        impl sandbox::Connectable for OutgoingConnector {
            fn send(&self, message: sandbox::Message) -> Result<(), ()> {
                let scope = ExecutionScope::new();
                let flags = fio::OpenFlags::empty();
                flags.to_object_request(message.channel).handle(|object_request| {
                    let path = vfs::path::Path::dot();
                    self.node.clone().open_entry(OpenRequest::new(
                        scope,
                        flags,
                        path,
                        object_request,
                    ))
                });
                Ok(())
            }
        }
        let node = Arc::new(SubNode::new(outgoing_dir_entry, relative_path, entry_type));
        let connector = sandbox::Connector::new_sendable(OutgoingConnector { node });
        let hook = CapabilityRequestedHook {
            source: component.as_weak(),
            name: name.clone(),
            connector,
            capability_decl: capability_decl.clone(),
        };
        match capability_decl {
            CapabilityDecl::Protocol(p) => match p.delivery {
                DeliveryType::Immediate => Router::<Connector>::new(hook),
                DeliveryType::OnReadable => hook.on_readable(component.execution_scope.clone()),
            },
            _ => Router::<Connector>::new(hook),
        }
    }

    /// Creates a `Router<DirEntry>` that requests the specified capability from the
    /// program's outgoing directory.
    pub fn make_program_outgoing_dir_entry_router(
        component: &Arc<ComponentInstance>,
        component_decl: &ComponentDecl,
        capability_decl: &cm_rust::CapabilityDecl,
    ) -> Router<DirEntry> {
        if component_decl.get_runner().is_none() {
            return Router::<DirEntry>::new_error(OpenOutgoingDirError::InstanceNonExecutable);
        }
        let name = capability_decl.name();
        let path = capability_decl.path().expect("must have path").to_string();
        let path = fuchsia_fs::canonicalize_path(&path);
        let entry_type = ComponentCapability::from(capability_decl.clone()).type_name().into();
        let relative_path = vfs::path::Path::validate_and_split(path).unwrap();
        let outgoing_dir_entry = component
            .get_outgoing()
            .try_into_directory_entry(component.execution_scope.clone())
            .expect("conversion to directory entry should succeed");
        let dir_entry =
            DirEntry::new(Arc::new(SubNode::new(outgoing_dir_entry, relative_path, entry_type)));
        // DirEntry-based capabilities don't need to support CapabilityRequested.
        Router::<DirEntry>::new(DirEntryOutgoingRouter {
            source: component.as_weak(),
            name: name.clone(),
            dir_entry,
            capability_decl: capability_decl.clone(),
        })
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
            component_input: self.sandbox.component_input.clone(),
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
                &self.sandbox.program_input.namespace(),
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

    fn extend_program_input_namespace_with_legacy(
        component: &Arc<ComponentInstance>,
        decl: &cm_rust::ComponentDecl,
        out_dict: &Dict,
    ) {
        let uses = Self::deduplicate_event_stream(decl.uses.iter());
        for use_ in uses {
            let path: cm_types::Path = match use_ {
                cm_rust::UseDecl::Protocol(d) => d.target_path.clone(),
                cm_rust::UseDecl::Service(d) => d.target_path.clone(),
                cm_rust::UseDecl::Directory(d) => d.target_path.clone(),
                cm_rust::UseDecl::Storage(d) => d.target_path.clone(),
                cm_rust::UseDecl::EventStream(d) => d.target_path.clone(),
                cm_rust::UseDecl::Runner(d) => format!("/{}", d.source_name).parse().unwrap(),
                cm_rust::UseDecl::Config(d) => format!("/{}", d.target_name).parse().unwrap(),
            };
            if !sandbox_construction::is_supported_use(&use_) {
                // Legacy capability.
                let request = RouteRequest::from(use_.clone());
                let Some(capability) = request.into_capability(component) else {
                    continue;
                };
                match out_dict.insert_capability(&path, capability) {
                    Ok(()) => {}
                    Err(e) => warn!("failed to insert {path} in program input dict: {e:?}"),
                };
            }
        }
    }

    /// This function transforms a sequence of [`UseDecl`] such that the duplicate event stream
    /// uses by paths are removed.
    ///
    /// Different from all other use declarations, multiple event stream capabilities may be used
    /// at the same path, the semantics being a single FIDL protocol capability is made available
    /// at that path, subscribing to all the specified events:
    /// see [`crate::model::events::registry::EventRegistry`].
    fn deduplicate_event_stream<'a>(
        iter: std::slice::Iter<'a, UseDecl>,
    ) -> impl Iterator<Item = &'a UseDecl> {
        let mut paths = HashSet::new();
        iter.filter_map(move |use_decl| match use_decl {
            UseDecl::EventStream(ref event_stream) => {
                if !paths.insert(event_stream.target_path.clone()) {
                    None
                } else {
                    Some(use_decl)
                }
            }
            _ => Some(use_decl),
        })
    }

    /// Returns a [`Dict`] with contents similar to `component_output_dict`, but adds capabilities
    /// backed by legacy routing, and hosts [`Open`]s instead of [`Router`]s. This [`Dict`] is used
    /// to generate the `exposed_dir`.
    pub async fn get_exposed_dict(&self) -> &Dict {
        let create_exposed_dict = async {
            let component = self.weak_component.upgrade().unwrap();
            let dict = sandbox::dict_routers_to_dir_entry(
                &component.execution_scope,
                &self.sandbox.component_output_dict,
            );
            Self::extend_exposed_dict_with_legacy(&component, self.decl(), &dict);
            dict
        };
        self.exposed_dict.get_or_init(create_exposed_dict).await
    }

    fn extend_exposed_dict_with_legacy(
        component: &Arc<ComponentInstance>,
        decl: &cm_rust::ComponentDecl,
        target_dict: &Dict,
    ) {
        // Filter out capabilities handled by bedrock routing
        let exposes = decl.exposes.iter().filter(|e| !sandbox_construction::is_supported_expose(e));
        let exposes_by_target_name = routing::aggregate_exposes(exposes);
        for (target_name, exposes) in exposes_by_target_name {
            let request =
                match routing::request_for_namespace_capability_expose(&component.moniker, exposes)
                {
                    Some(r) => r,
                    None => continue,
                };
            let Some(capability) = request.into_capability(component) else {
                continue;
            };
            match target_dict.insert_capability(target_name, capability) {
                Ok(()) => (),
                Err(e) => warn!("failed to insert {} in target dict: {e:?}", target_name),
            };
        }
    }

    pub async fn get_exposed_dir(&self) -> &DirEntry {
        let create_exposed_dir = async {
            let exposed_dict = self.get_exposed_dict().await.clone();
            DirEntry::new(
                exposed_dict
                    .try_into_directory_entry(self.execution_scope.clone())
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

        // Delete any dynamic offers whose `source` or `target` matches the
        // component we're deleting.
        self.dynamic_offers.retain(|offer| {
            let source_matches = offer.source()
                == &cm_rust::OfferSource::Child(cm_rust::ChildRef {
                    name: moniker.name().clone(),
                    collection: moniker.collection().map(|c| c.clone()),
                });
            let target_matches = offer.target()
                == &cm_rust::OfferTarget::Child(cm_rust::ChildRef {
                    name: moniker.name().clone(),
                    collection: moniker.collection().map(|c| c.clone()),
                });
            !source_matches && !target_matches
        });
    }

    /// Creates a set of Environments instantiated from their EnvironmentDecls.
    fn instantiate_environments(
        component: &Arc<ComponentInstance>,
        decl: &ComponentDecl,
    ) -> HashMap<Name, Arc<Environment>> {
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
        environment_name: Option<&Name>,
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

    /// Adds a new child component instance.
    pub async fn add_child(
        &mut self,
        component: &Arc<ComponentInstance>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
        dynamic_offers: Option<Vec<fdecl::Offer>>,
        controller: Option<ServerEnd<fcomponent::ControllerMarker>>,
        input: ComponentInput,
    ) -> Result<Arc<ComponentInstance>, AddDynamicChildError> {
        let child =
            self.add_child_internal(component, child, collection, dynamic_offers, input).await?;

        if let Some(controller) = controller {
            let stream = controller.into_stream();
            child
                .nonblocking_task_group()
                .spawn(controller::run_controller(WeakComponentInstance::new(&child), stream));
        }
        Ok(child)
    }

    async fn add_child_internal(
        &mut self,
        component: &Arc<ComponentInstance>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
        dynamic_offers: Option<Vec<fdecl::Offer>>,
        child_input: ComponentInput,
    ) -> Result<Arc<ComponentInstance>, AddChildError> {
        assert!(
            (dynamic_offers.is_none()) || collection.is_some(),
            "setting numbered handles or dynamic offers for static children",
        );
        let dynamic_offers =
            self.validate_and_convert_dynamic_component(dynamic_offers, child, collection)?;

        let child_name =
            ChildName::try_new(child.name.as_str(), collection.map(|c| c.name.as_str()))?;

        let child_component_output_dictionary_routers =
            self.get_child_component_output_dictionary_routers();
        if !dynamic_offers.is_empty() {
            extend_dict_with_offers(
                &component,
                &child_component_output_dictionary_routers,
                &self.sandbox.component_input,
                &dynamic_offers,
                &self.sandbox.program_output_dict,
                &self.sandbox.framework_dict,
                &self.sandbox.capability_sourced_capabilities_dict,
                &child_input,
                RoutingFailureErrorReporter::new(component.as_weak()),
            );
        }

        if self.get_child(&child_name).is_some() {
            return Err(AddChildError::InstanceAlreadyExists {
                moniker: component.moniker().clone(),
                child: child_name,
            });
        }
        let incarnation_id = match collection {
            Some(_) => {
                let id = self.next_dynamic_instance_id;
                self.next_dynamic_instance_id += 1;
                id
            }
            None => 0,
        };
        let child = ComponentInstance::new(
            child_input,
            self.environment_for_child(component, child, collection.clone()),
            component.moniker.child(child_name.clone()),
            incarnation_id,
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
        self.children.insert(child_name, child.clone());

        self.dynamic_offers.extend(dynamic_offers.into_iter());
        Ok(child)
    }

    fn add_target_dynamic_offers(
        &self,
        mut dynamic_offers: Vec<fdecl::Offer>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
    ) -> Result<Vec<fdecl::Offer>, DynamicCapabilityError> {
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
                        return Err(DynamicCapabilityError::Invalid {
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
                    return Err(DynamicCapabilityError::UnknownOfferType);
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
        all_dynamic_children: Vec<(&str, &str)>,
        dynamic_offers: Vec<fdecl::Offer>,
    ) -> Result<(), AddChildError> {
        // Combine all our dynamic offers.
        let mut all_dynamic_offers: Vec<_> =
            self.dynamic_offers.clone().into_iter().map(NativeIntoFidl::native_into_fidl).collect();
        all_dynamic_offers.append(&mut dynamic_offers.clone());

        // Validate!
        cm_fidl_validator::validate_dynamic_offers(
            all_dynamic_children,
            &all_dynamic_offers,
            &self.resolved_component.decl.clone().native_into_fidl(),
        )
        .map_err(|err| {
            if err.errs.iter().all(|e| matches!(e, ValidatorError::DependencyCycle(_))) {
                DynamicCapabilityError::Cycle { err }
            } else {
                DynamicCapabilityError::Invalid { err }
            }
        })?;

        // Manifest validation is not informed of the contents of collections, and is thus unable
        // to confirm the source exists if it's in a collection. Let's check that here.
        let dynamic_offers: Vec<cm_rust::OfferDecl> =
            dynamic_offers.into_iter().map(FidlIntoNative::fidl_into_native).collect();
        for offer in &dynamic_offers {
            if !self.offer_source_exists(offer.source()) {
                return Err(DynamicCapabilityError::SourceNotFound { offer: offer.clone() }.into());
            }
        }
        Ok(())
    }

    pub fn validate_and_convert_dynamic_component(
        &self,
        dynamic_offers: Option<Vec<fdecl::Offer>>,
        child: &ChildDecl,
        collection: Option<&CollectionDecl>,
    ) -> Result<Vec<cm_rust::OfferDecl>, AddChildError> {
        let dynamic_offers = dynamic_offers.unwrap_or_default();

        let dynamic_offers = self.add_target_dynamic_offers(dynamic_offers, child, collection)?;
        if !dynamic_offers.is_empty() {
            let mut all_dynamic_children: Vec<_> = self
                .children()
                .filter_map(|(n, _)| {
                    if let Some(collection) = n.collection() {
                        Some((n.name().as_str(), collection.as_str()))
                    } else {
                        None
                    }
                })
                .collect();
            all_dynamic_children.push((
                child.name.as_str(),
                collection
                    .expect("child must be dynamic if there were dynamic offers")
                    .name
                    .as_str(),
            ));
            self.validate_dynamic_component(all_dynamic_children, dynamic_offers.clone())?;
        }
        Ok(dynamic_offers.into_iter().map(|o| o.fidl_into_native()).collect())
    }

    async fn add_static_children(
        &mut self,
        component: &Arc<ComponentInstance>,
    ) -> Result<(), ResolveActionError> {
        // We can't hold an immutable reference to `self` while passing a mutable reference later
        // on. To get around this, clone the children.
        let children = self.resolved_component.decl.children.clone();
        for child in &children {
            // `child_input` will be populated later, after the component's sandbox is
            // constructed.
            self.add_child_internal(component, child, None, None, ComponentInput::default())
                .await
                .map_err(|err| ResolveActionError::AddStaticChildError {
                    child_name: child.name.to_string(),
                    err,
                })?;
        }
        Ok(())
    }

    async fn populate_child_inputs(&self, child_inputs: &StructuredDictMap<ComponentInput>) {
        for (child_name, child_instance) in &self.children {
            if let Some(_) = child_name.collection {
                continue;
            }
            let child_name =
                Name::new(child_name.name.as_str()).expect("child is static so name is not long");
            let child_input = child_inputs.get(&child_name).expect("missing child dict");
            let mut state = child_instance.lock_state().await;
            let InstanceState::Unresolved(state) = &mut *state else {
                unreachable!("still building sandbox, the child can't be resolved yet");
            };
            let _ = std::mem::replace(&mut state.component_input, child_input);
        }
    }

    fn get_child_component_output_dictionary_routers(&self) -> HashMap<ChildName, Router<Dict>> {
        self.children.iter().map(|(name, child)| (name.clone(), child.component_output())).collect()
    }

    pub fn moniker(&self) -> &Moniker {
        &self.weak_component.moniker
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
        self.resolved_component.decl.capabilities.clone()
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

impl ProgramRuntime {
    pub fn new(program: Program, component: WeakComponentInstance) -> Self {
        let terminated_fut = program.on_terminate();
        let exit_listener = fasync::Task::spawn(async move {
            terminated_fut.await;
            if let Ok(component) = component.upgrade() {
                let stop_nf = component.actions().register_no_wait(StopAction::new(false)).await;
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
    ) -> Result<StopConclusion, StopError> {
        // Drop the program and join on the exit listener. Dropping the program
        // should cause the exit listener to stop waiting for the channel epitaph and
        // exit.
        //
        // Note: this is more reliable than just cancelling `exit_listener` because
        // even after cancellation future may still run for a short period of time
        // before getting dropped. If that happens there is a chance of scheduling a
        // duplicate Stop action.
        let res = self.program.stop_or_kill_with_timeout(stop_timer, kill_timer).await;
        self.exit_listener.await;
        res
    }
}

/// The execution state for a component instance that has started running.
///
/// If the component instance has a program, it may also have a [`ProgramRuntime`].
pub struct StartedInstanceState {
    /// If set, that means this component is associated with a running program.
    program: Option<ProgramRuntime>,

    /// Approximates when the component was started in nanoseconds since boot.
    pub timestamp: zx::BootInstant,

    /// Approximates when the component was started in monotonic time. This time doesn't measure
    /// the time since boot and won't include time the system spent suspended.
    pub timestamp_monotonic: zx::MonotonicInstant,

    /// Describes why the component instance was started
    pub start_reason: StartReason,

    /// Channels scoped to lifetime of this component's execution context. This
    /// should only be used for the server_end of the `fuchsia.component.Binder`
    /// connection.
    binder_server_ends: Vec<zx::Channel>,

    /// This stores the hook for notifying an ExecutionController about stop events for this
    /// component.
    execution_controller_task: Option<controller::ExecutionControllerTask>,

    /// Logger attributed to this component.
    ///
    /// Only set if the component uses the `fuchsia.logger.LogSink` protocol.
    pub logger: Option<Arc<ScopedLogger>>,
}

impl StartedInstanceState {
    /// Creates the state corresponding to a started component.
    ///
    /// If `program` is present, also creates a background task waiting for the program to
    /// terminate. When that happens, uses the [`WeakComponentInstance`] to stop the component.
    pub fn new(
        program: Option<Program>,
        component: WeakComponentInstance,
        start_reason: StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        logger: Option<ScopedLogger>,
    ) -> Self {
        let timestamp = zx::BootInstant::get();
        let timestamp_monotonic = zx::MonotonicInstant::get();
        StartedInstanceState {
            program: program.map(|p| ProgramRuntime::new(p, component)),
            timestamp,
            timestamp_monotonic,
            binder_server_ends: vec![],
            start_reason,
            execution_controller_task,
            logger: logger.map(Arc::new),
        }
    }

    /// If this component has a program, obtain a capability representing its runtime directory.
    pub fn runtime_dir(&self) -> Option<&fio::DirectoryProxy> {
        self.program.as_ref().map(|program_runtime| program_runtime.program.runtime())
    }

    /// Stops the component. If the component has a program, the timer defines how long
    /// the runner is given to stop the program gracefully before we request the controller
    /// to terminate the program.
    pub async fn stop<'a, 'b>(
        mut self,
        stop_timer: BoxFuture<'a, ()>,
        kill_timer: BoxFuture<'b, ()>,
    ) -> Result<StopConclusion, StopError> {
        let program = self.program.take();
        // If the component has a program, also stop the program.
        let ret = if let Some(program) = program {
            program.stop(stop_timer, kill_timer).await
        } else {
            Ok(StopConclusion { disposition: StopDisposition::NoController, escrow_request: None })
        }?;
        if let Some(execution_controller_task) = self.execution_controller_task.as_mut() {
            execution_controller_task.set_stop_payload(ret.disposition.stop_info());
        }
        Ok(ret)
    }

    /// Add a channel scoped to the lifetime of this object.
    pub fn add_scoped_server_end(&mut self, server_end: zx::Channel) {
        self.binder_server_ends.push(server_end);
    }

    /// Gets a [`Koid`] that will uniquely identify the program.
    #[cfg(test)]
    pub fn program_koid(&self) -> Option<zx::Koid> {
        self.program.as_ref().map(|program_runtime| program_runtime.program.koid())
    }
}

/// This delegates to the event system if the capability request is
/// intercepted by some hook, and delegates to the current capability otherwise.
#[derive(Debug)]
struct CapabilityRequestedHook {
    source: WeakComponentInstance,
    name: Name,
    connector: Connector,
    capability_decl: cm_rust::CapabilityDecl,
}

#[async_trait]

impl Routable<Connector> for CapabilityRequestedHook {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<Connector>, RouterError> {
        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;

        fn cm_unexpected() -> RouterError {
            RoutingError::from(ComponentInstanceError::ComponentManagerInstanceUnexpected {}).into()
        }

        let ExtendedMoniker::ComponentInstance(target_moniker) =
            <WeakInstanceToken as WeakInstanceTokenExt<ComponentInstance>>::moniker(
                &request.target,
            )
        else {
            return Err(cm_unexpected());
        };
        self.source
            .ensure_started(&StartReason::AccessCapability {
                target: target_moniker,
                name: self.name.clone(),
            })
            .await?;
        let source = self.source.upgrade().map_err(RoutingError::from)?;
        let ExtendedInstance::Component(target) =
            request.target.upgrade().map_err(RoutingError::from)?
        else {
            return Err(cm_unexpected());
        };
        let (receiver, sender) = CapabilityReceiver::new();
        let event = target.new_event(EventPayload::CapabilityRequested {
            source_moniker: source.moniker.clone(),
            name: self.name.to_string(),
            receiver: receiver.clone(),
        });
        source.hooks.dispatch(&event).await;
        let resp = if debug {
            RouterResponse::<Connector>::Debug(
                CapabilitySource::Component(ComponentSource {
                    capability: self.capability_decl.clone().into(),
                    moniker: self.source.moniker.clone(),
                })
                .try_into()
                .expect("failed to convert capability source to Data"),
            )
        } else if receiver.is_taken() {
            RouterResponse::<Connector>::Capability(sender)
        } else {
            RouterResponse::<Connector>::Capability(self.connector.clone())
        };
        Ok(resp)
    }
}

#[derive(Debug)]
struct DirEntryOutgoingRouter {
    source: WeakComponentInstance,
    name: Name,
    dir_entry: DirEntry,
    capability_decl: cm_rust::CapabilityDecl,
}

#[async_trait]
impl Routable<DirEntry> for DirEntryOutgoingRouter {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<DirEntry>, RouterError> {
        fn cm_unexpected() -> RouterError {
            RoutingError::from(ComponentInstanceError::ComponentManagerInstanceUnexpected {}).into()
        }

        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
        let ExtendedMoniker::ComponentInstance(target_moniker) =
            <WeakInstanceToken as WeakInstanceTokenExt<ComponentInstance>>::moniker(
                &request.target,
            )
        else {
            return Err(cm_unexpected());
        };
        self.source
            .ensure_started(&StartReason::AccessCapability {
                target: target_moniker,
                name: self.name.clone(),
            })
            .await?;
        let ExtendedInstance::Component(_) =
            request.target.upgrade().map_err(RoutingError::from)?
        else {
            return Err(cm_unexpected());
        };
        let resp = if debug {
            RouterResponse::<DirEntry>::Debug(
                CapabilitySource::Component(ComponentSource {
                    capability: self.capability_decl.clone().into(),
                    moniker: self.source.moniker.clone(),
                })
                .try_into()
                .expect("failed to convert capability source to Data"),
            )
        } else {
            RouterResponse::<DirEntry>::Capability(self.dir_entry.clone())
        };
        Ok(resp)
    }
}

struct ProgramDictionaryRouter {
    component: WeakComponentInstance,
    source_path: Path,
    capability: ComponentCapability,
}

#[async_trait]
impl Routable<Dict> for ProgramDictionaryRouter {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<Dict>, RouterError> {
        if debug {
            let source = CapabilitySource::Component(ComponentSource {
                capability: self.capability.clone(),
                moniker: self.component.moniker.clone(),
            });
            let cap = Capability::try_from(source)
                .expect("failed to convert capability source to capability");
            let Capability::Data(data) = cap else {
                panic!("failed to convert capability source to Debug");
            };
            return Ok(RouterResponse::<Dict>::Debug(data));
        }
        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
        fn open_error(e: OpenOutgoingDirError) -> OpenError {
            CapabilityProviderError::from(ComponentProviderError::from(e)).into()
        }

        let component = self.component.upgrade().map_err(|_| {
            RoutingError::from(ComponentInstanceError::instance_not_found(
                self.component.moniker.clone(),
            ))
        })?;
        let dir_entry = component.get_outgoing();

        let (inner_router, server_end) =
            create_proxy::<fsandbox::DictionaryRouterMarker>().unwrap();
        dir_entry.open(
            ExecutionScope::new(),
            fio::OpenFlags::empty(),
            vfs::path::Path::validate_and_split(self.source_path.to_string())
                .expect("path must be valid"),
            server_end.into_channel(),
        );
        let resp = inner_router
            .route(request.into())
            .await
            .map_err(|e| open_error(OpenOutgoingDirError::Fidl(e)))?
            .map_err(RouterError::from)?;
        resp.try_into().map_err(|e| RouterError::NotFound(Arc::new(e)))
    }
}
