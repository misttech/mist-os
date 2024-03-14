// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        bedrock::program::{self as program, ComponentStopOutcome, Program, StopRequestSuccess},
        framework::controller,
        model::{
            actions::{
                resolve::sandbox_construction::{
                    self, build_component_sandbox, extend_dict_with_offers, ComponentEnvironment,
                    ComponentInput,
                },
                shutdown, start, ActionSet, DestroyAction, DiscoverAction, ResolveAction,
                ShutdownAction, ShutdownType, StartAction, StopAction, UnresolveAction,
            },
            context::ModelContext,
            environment::Environment,
            error::{
                ActionError, AddChildError, AddDynamicChildError, CreateNamespaceError,
                DestroyActionError, DynamicOfferError, ModelError, OpenExposedDirError,
                OpenOutgoingDirError, RebootError, ResolveActionError, StartActionError,
                StopActionError, StructuredConfigError,
            },
            hooks::{CapabilityReceiver, Event, EventPayload, Hooks},
            namespace::create_namespace,
            routing::{
                self,
                router::{Request, Routable, Router},
                service::{AnonymizedAggregateServiceDir, AnonymizedServiceRoute},
                RoutingError,
            },
            routing_fns::route_fn,
            start::Start,
            token::{InstanceToken, InstanceTokenState},
        },
        sandbox_util::DictExt,
    },
    ::namespace::Entry as NamespaceEntry,
    ::routing::{
        capability_source::{BuiltinCapabilities, ComponentCapability, NamespaceCapabilities},
        component_instance::{
            ComponentInstanceInterface, ExtendedInstanceInterface, ResolvedInstanceInterface,
            ResolvedInstanceInterfaceExt, TopInstanceInterface, WeakComponentInstanceInterface,
            WeakExtendedInstanceInterface,
        },
        environment::EnvironmentInterface,
        error::ComponentInstanceError,
        policy::GlobalPolicyChecker,
        resolving::{
            ComponentAddress, ComponentResolutionContext, ResolvedComponent, ResolvedPackage,
        },
    },
    async_trait::async_trait,
    async_utils::async_once::Once,
    bedrock_error::Explain,
    clonable_error::ClonableError,
    cm_fidl_validator::error::DeclType,
    cm_logger::scoped::ScopedLogger,
    cm_moniker::{IncarnationId, InstancedChildName, InstancedMoniker},
    cm_rust::{
        CapabilityDecl, CapabilityTypeName, ChildDecl, CollectionDecl, ComponentDecl,
        FidlIntoNative, NativeIntoFidl, OfferDeclCommon, SourceName, UseDecl,
    },
    cm_types::Name,
    cm_util::{channel, TaskGroup},
    component_id_index::InstanceId,
    config_encoder::ConfigFields,
    fidl::{
        endpoints::{self, ServerEnd},
        epitaph::ChannelEpitaphExt,
    },
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_hardware_power_statecontrol as fstatecontrol, fidl_fuchsia_io as fio,
    fidl_fuchsia_process as fprocess, fuchsia_async as fasync,
    fuchsia_component::client,
    fuchsia_zircon as zx,
    futures::{
        future::{join_all, BoxFuture, FutureExt},
        lock::{MappedMutexGuard, Mutex, MutexGuard},
    },
    moniker::{ChildName, ChildNameBase, Moniker, MonikerBase},
    sandbox::{Capability, CapabilityTrait, Dict, Open},
    std::{
        clone::Clone,
        collections::{HashMap, HashSet},
        fmt, iter,
        sync::{Arc, Weak},
        time::Duration,
    },
    tracing::{debug, warn},
    version_history::AbiRevision,
    vfs::{directory::immutable::simple as pfs, execution_scope::ExecutionScope, path::Path},
};

pub type WeakComponentInstance = WeakComponentInstanceInterface<ComponentInstance>;
pub type ExtendedInstance = ExtendedInstanceInterface<ComponentInstance>;
pub type WeakExtendedInstance = WeakExtendedInstanceInterface<ComponentInstance>;

/// Describes the reason a component instance is being requested to start.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum StartReason {
    /// Indicates that the target is starting the component because it wishes to access
    /// the capability at path.
    AccessCapability { target: Moniker, name: Name },
    /// Indicates that the component is starting because it is in a single-run collection.
    SingleRun,
    /// Indicates that the component was explicitly started for debugging purposes.
    Debug,
    /// Indicates that the component was marked as eagerly starting by the parent.
    // TODO(https://fxbug.dev/42127825): Include the parent StartReason.
    // parent: ExtendedMoniker,
    // parent_start_reason: Option<Arc<StartReason>>
    Eager,
    /// Indicates that this component is starting because it is the root component.
    Root,
    /// Storage administration is occurring on this component.
    StorageAdmin,
    /// Indicates that this component is starting because the client of a
    /// `fuchsia.component.Controller` connection has called `Start()`
    Controller,
}

impl fmt::Display for StartReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                StartReason::AccessCapability { target, name } => {
                    format!("'{}' requested capability '{}'", target, name)
                }
                StartReason::SingleRun => "Instance is in a single_run collection".to_string(),
                StartReason::Debug => "Instance was started from debugging workflow".to_string(),
                StartReason::Eager => "Instance is an eager child".to_string(),
                StartReason::Root => "Instance is the root".to_string(),
                StartReason::StorageAdmin => "Storage administration on instance".to_string(),
                StartReason::Controller =>
                    "Instructed to start with the fuchsia.component.Controller protocol".to_string(),
            }
        )
    }
}

/// Component information returned by the resolver.
#[derive(Clone, Debug)]
pub struct Component {
    /// The URL of the resolved component.
    pub resolved_url: String,
    /// The context to be used to resolve a component from a path
    /// relative to this component (for example, a component in a subpackage).
    /// If `None`, the resolver cannot resolve relative path component URLs.
    pub context_to_resolve_children: Option<ComponentResolutionContext>,
    /// The declaration of the resolved manifest.
    pub decl: ComponentDecl,
    /// The package info, if the component came from a package.
    pub package: Option<Package>,
    /// The component's validated configuration. If None, no configuration was provided.
    pub config: Option<ConfigFields>,
    /// The component's target ABI revision, if available.
    pub abi_revision: Option<AbiRevision>,
}

impl Component {
    pub fn resolve_with_config(
        ResolvedComponent {
            resolved_by: _,
            resolved_url,
            context_to_resolve_children,
            decl,
            package,
            config_values,
            abi_revision,
        }: ResolvedComponent,
        config_parent_overrides: Option<&Vec<cm_rust::ConfigOverride>>,
    ) -> Result<Self, ResolveActionError> {
        let config = if let Some(config_decl) = decl.config.as_ref() {
            match config_decl.value_source {
                // If the config is provided via routing then `config_values` will be empty.
                cm_rust::ConfigValueSource::Capabilities(_) => None,
                // If the config is provided in our package then the resolver should give use the values.
                cm_rust::ConfigValueSource::PackagePath(_) => {
                    let values = config_values.ok_or(StructuredConfigError::ConfigValuesMissing)?;
                    let config =
                        ConfigFields::resolve(config_decl, values, config_parent_overrides)
                            .map_err(StructuredConfigError::ConfigResolutionFailed)?;
                    Some(config)
                }
            }
        } else {
            None
        };

        let package = package.map(|p| p.try_into()).transpose()?;
        Ok(Self { resolved_url, context_to_resolve_children, decl, package, config, abi_revision })
    }
}

/// Package information possibly returned by the resolver.
#[derive(Clone, Debug)]
pub struct Package {
    /// The URL of the package itself.
    pub package_url: String,
    /// The package that this resolved component belongs to
    pub package_dir: fio::DirectoryProxy,
}

impl TryFrom<ResolvedPackage> for Package {
    type Error = ResolveActionError;

    fn try_from(package: ResolvedPackage) -> Result<Self, Self::Error> {
        Ok(Self {
            package_url: package.url,
            package_dir: package
                .directory
                .into_proxy()
                .map_err(|err| ResolveActionError::PackageDirProxyCreateError { err })?,
        })
    }
}

pub const DEFAULT_KILL_TIMEOUT: Duration = Duration::from_secs(1);

/// A special instance identified with component manager, at the top of the tree.
#[derive(Debug)]
pub struct ComponentManagerInstance {
    /// The list of capabilities offered from component manager's namespace.
    pub namespace_capabilities: NamespaceCapabilities,

    /// The list of capabilities offered from component manager as built-in capabilities.
    pub builtin_capabilities: BuiltinCapabilities,

    /// Tasks owned by component manager's instance.
    task_group: TaskGroup,

    /// Mutable state for component manager's instance.
    state: Mutex<ComponentManagerInstanceState>,
}

/// Mutable state for component manager's instance.
pub struct ComponentManagerInstanceState {
    /// The root component instance, this instance's only child.
    root: Option<Arc<ComponentInstance>>,

    /// Task that is rebooting the system, if any.
    reboot_task: Option<fasync::Task<()>>,
}

impl ComponentManagerInstance {
    pub fn new(
        namespace_capabilities: NamespaceCapabilities,
        builtin_capabilities: BuiltinCapabilities,
    ) -> Self {
        Self {
            namespace_capabilities,
            builtin_capabilities,
            state: Mutex::new(ComponentManagerInstanceState::new()),
            task_group: TaskGroup::new(),
        }
    }

    /// Returns a group where tasks can be run scoped to this instance
    pub fn task_group(&self) -> TaskGroup {
        self.task_group.clone()
    }

    #[cfg(test)]
    pub async fn has_reboot_task(&self) -> bool {
        self.state.lock().await.reboot_task.is_some()
    }

    /// Returns the root component instance.
    ///
    /// REQUIRES: The root has already been set. Otherwise panics.
    pub async fn root(&self) -> Arc<ComponentInstance> {
        self.state.lock().await.root.as_ref().expect("root not set").clone()
    }

    /// Initializes the state of the instance. Panics if already initialized.
    pub(super) async fn init(&self, root: Arc<ComponentInstance>) {
        let mut state = self.state.lock().await;
        assert!(state.root.is_none(), "child of top instance already set");
        state.root = Some(root);
    }

    /// Triggers a graceful system reboot. Panics if the reboot call fails (which will trigger a
    /// forceful reboot if this is the root component manager instance).
    ///
    /// Returns as soon as the call has been made. In the background, component_manager will wait
    /// on the `Reboot` call.
    pub(super) async fn trigger_reboot(self: &Arc<Self>) {
        let mut state = self.state.lock().await;
        if state.reboot_task.is_some() {
            // Reboot task was already scheduled, nothing to do.
            return;
        }
        let this = self.clone();
        state.reboot_task = Some(fasync::Task::spawn(async move {
            let res = async move {
                let statecontrol_proxy = this.connect_to_statecontrol_admin().await?;
                statecontrol_proxy
                    .reboot(fstatecontrol::RebootReason::CriticalComponentFailure)
                    .await?
                    .map_err(|s| RebootError::AdminError(zx::Status::from_raw(s)))
            }
            .await;
            if let Err(e) = res {
                // TODO(https://fxbug.dev/42161535): Instead of panicking, we could fall back more gently by
                // triggering component_manager's shutdown.
                panic!(
                    "Component with on_terminate=REBOOT terminated, but triggering \
                    reboot failed. Crashing component_manager instead: {}",
                    e
                );
            }
        }));
    }

    /// Obtains a connection to power_manager's `statecontrol` protocol.
    async fn connect_to_statecontrol_admin(
        &self,
    ) -> Result<fstatecontrol::AdminProxy, RebootError> {
        let (exposed_dir, server) =
            endpoints::create_proxy::<fio::DirectoryMarker>().expect("failed to create proxy");
        let mut server = server.into_channel();
        let root = self.root().await;
        root.open_exposed(&mut server).await?;
        let statecontrol_proxy =
            client::connect_to_protocol_at_dir_root::<fstatecontrol::AdminMarker>(&exposed_dir)
                .map_err(RebootError::ConnectToAdminFailed)?;
        Ok(statecontrol_proxy)
    }
}

impl ComponentManagerInstanceState {
    pub fn new() -> Self {
        Self { reboot_task: None, root: None }
    }
}

impl TopInstanceInterface for ComponentManagerInstance {
    fn namespace_capabilities(&self) -> &NamespaceCapabilities {
        &self.namespace_capabilities
    }

    fn builtin_capabilities(&self) -> &BuiltinCapabilities {
        &self.builtin_capabilities
    }
}

/// Capabilities that a component receives dynamically.
pub struct IncomingCapabilities {
    pub numbered_handles: Vec<fprocess::HandleInfo>,
    pub additional_namespace_entries: Vec<NamespaceEntry>,
    pub dict: Option<sandbox::Dict>,
}

impl Default for IncomingCapabilities {
    fn default() -> Self {
        Self { numbered_handles: Vec::new(), additional_namespace_entries: Vec::new(), dict: None }
    }
}

/// Models a component instance, possibly with links to children.
pub struct ComponentInstance {
    /// The registry for resolving component URLs within the component instance.
    pub environment: Arc<Environment>,
    /// The component's URL.
    pub component_url: String,
    /// The mode of startup (lazy or eager).
    pub startup: fdecl::StartupMode,
    /// The policy to apply if the component terminates.
    pub on_terminate: fdecl::OnTerminate,
    /// The parent instance. Either a component instance or component manager's instance.
    pub parent: WeakExtendedInstance,
    /// The instanced moniker of this instance.
    instanced_moniker: InstancedMoniker,
    /// The partial moniker of this instance.
    pub moniker: Moniker,
    /// The hooks scoped to this instance.
    pub hooks: Arc<Hooks>,
    /// Whether to persist isolated storage data of this component instance after it has been
    /// destroyed.
    pub persistent_storage: bool,

    /// Configuration overrides provided by the parent component.
    config_parent_overrides: Option<Vec<cm_rust::ConfigOverride>>,

    /// The context shared across the model.
    pub context: Arc<ModelContext>,

    // These locks must be taken in the order declared if held simultaneously.
    /// The component's mutable state.
    state: Mutex<InstanceState>,
    /// The component's execution state.
    execution: Mutex<ExecutionState>,
    /// Actions on the instance that must eventually be completed.
    actions: Mutex<ActionSet>,
    /// Tasks owned by this component instance that will be cancelled if the component is
    /// destroyed.
    nonblocking_task_group: TaskGroup,
    /// Tasks owned by this component instance that will block destruction if the component is
    /// destroyed.
    blocking_task_group: TaskGroup,
}

impl ComponentInstance {
    /// Instantiates a new root component instance.
    pub fn new_root(
        environment: Environment,
        context: Arc<ModelContext>,
        component_manager_instance: Weak<ComponentManagerInstance>,
        component_url: String,
    ) -> Arc<Self> {
        Self::new(
            Arc::new(environment),
            InstancedMoniker::root(),
            component_url,
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            context,
            WeakExtendedInstance::AboveRoot(component_manager_instance),
            Arc::new(Hooks::new()),
            false,
        )
    }

    /// Instantiates a new component instance with the given contents.
    // TODO(https://fxbug.dev/42077692) convert this to a builder API
    pub fn new(
        environment: Arc<Environment>,
        instanced_moniker: InstancedMoniker,
        component_url: String,
        startup: fdecl::StartupMode,
        on_terminate: fdecl::OnTerminate,
        config_parent_overrides: Option<Vec<cm_rust::ConfigOverride>>,
        context: Arc<ModelContext>,
        parent: WeakExtendedInstance,
        hooks: Arc<Hooks>,
        persistent_storage: bool,
    ) -> Arc<Self> {
        let moniker = instanced_moniker.without_instance_ids();
        Arc::new(Self {
            environment,
            instanced_moniker,
            moniker,
            component_url,
            startup,
            on_terminate,
            config_parent_overrides,
            context,
            parent,
            state: Mutex::new(InstanceState::New),
            execution: Mutex::new(ExecutionState::new()),
            actions: Mutex::new(ActionSet::new()),
            hooks,
            nonblocking_task_group: TaskGroup::new(),
            blocking_task_group: TaskGroup::new(),
            persistent_storage,
        })
    }

    /// Locks and returns the instance's mutable state.
    // TODO(b/309656051): Remove this method from ComponentInstance's public API
    pub async fn lock_state(&self) -> MutexGuard<'_, InstanceState> {
        self.state.lock().await
    }

    /// Locks and returns the instance's execution state.
    // TODO(b/309656051): Remove this method from ComponentInstance's public API
    pub async fn lock_execution(&self) -> MutexGuard<'_, ExecutionState> {
        self.execution.lock().await
    }

    /// Locks and returns the instance's action set.
    // TODO(b/309656051): Remove this method from ComponentInstance's public API
    pub async fn lock_actions(&self) -> MutexGuard<'_, ActionSet> {
        self.actions.lock().await
    }

    /// Returns a group for this instance where tasks can be run scoped to this instance. Tasks run
    /// in this group will be cancelled when the component is destroyed.
    pub fn nonblocking_task_group(&self) -> TaskGroup {
        self.nonblocking_task_group.clone()
    }

    /// Returns a group for this instance where tasks can be run scoped to this instance. Tasks run
    /// in this group will block destruction if the component is destroyed.
    pub fn blocking_task_group(&self) -> TaskGroup {
        self.blocking_task_group.clone()
    }

    /// Returns true if the component is started, i.e. when it has a runtime.
    pub async fn is_started(&self) -> bool {
        self.lock_execution().await.is_started()
    }

    /// Locks and returns a lazily resolved and populated `ResolvedInstanceState`. Does not
    /// register a `Resolve` action unless the resolved state is not already populated, so this
    /// function can be called re-entrantly from a Resolved hook. Returns an `InstanceNotFound`
    /// error if the instance is destroyed.
    // TODO(b/309656051): Remove this method from ComponentInstance's public API
    pub async fn lock_resolved_state<'a>(
        self: &'a Arc<Self>,
    ) -> Result<MappedMutexGuard<'a, InstanceState, ResolvedInstanceState>, ActionError> {
        fn get_resolved(s: &mut InstanceState) -> &mut ResolvedInstanceState {
            match s {
                InstanceState::Resolved(s) => s,
                _ => panic!("not resolved"),
            }
        }
        {
            let state = self.state.lock().await;
            match *state {
                InstanceState::Resolved(_) => {
                    return Ok(MutexGuard::map(state, get_resolved));
                }
                InstanceState::Destroyed => {
                    return Err(ResolveActionError::InstanceDestroyed {
                        moniker: self.moniker.clone(),
                    }
                    .into());
                }
                InstanceState::New | InstanceState::Unresolved(_) => {}
            }
            // Drop the lock before doing the work to resolve the state.
        }
        self.resolve().await?;
        let state = self.state.lock().await;
        if let InstanceState::Destroyed = *state {
            return Err(
                ResolveActionError::InstanceDestroyed { moniker: self.moniker.clone() }.into()
            );
        }
        Ok(MutexGuard::map(state, get_resolved))
    }

    /// Resolves the component declaration, populating `ResolvedInstanceState` as necessary. A
    /// `Resolved` event is dispatched if the instance was not previously resolved or an error
    /// occurs.
    pub async fn resolve(self: &Arc<Self>) -> Result<(), ActionError> {
        ActionSet::register(self.clone(), ResolveAction::new()).await
    }

    /// Unresolves the component using an UnresolveAction. The component will be shut down, then
    /// reset to the Discovered state without being destroyed. An Unresolved event is dispatched on
    /// success or error.
    pub async fn unresolve(self: &Arc<Self>) -> Result<(), ActionError> {
        ActionSet::register(self.clone(), UnresolveAction::new()).await
    }

    /// Adds the dynamic child defined by `child_decl` to the given `collection_name`.
    pub async fn add_dynamic_child(
        self: &Arc<Self>,
        collection_name: String,
        child_decl: &ChildDecl,
        child_args: fcomponent::CreateChildArgs,
    ) -> Result<(), AddDynamicChildError> {
        let mut state = self.lock_resolved_state().await?;
        let collection_decl = state
            .decl()
            .find_collection(&collection_name)
            .ok_or_else(|| AddDynamicChildError::CollectionNotFound {
                name: collection_name.clone(),
            })?
            .clone();
        let is_single_run_collection = collection_decl.durability == fdecl::Durability::SingleRun;
        // Start the child if it's created in a `SingleRun` collection or it's eager.
        let maybe_start_reason = if is_single_run_collection {
            Some(StartReason::SingleRun)
        } else if child_decl.startup == fdecl::StartupMode::Eager {
            Some(StartReason::Eager)
        } else {
            None
        };

        // Specifying numbered handles is only allowed if the component is started in
        // a single-run collection.
        let numbered_handles = child_args.numbered_handles.unwrap_or_default();
        if !is_single_run_collection && !numbered_handles.is_empty() {
            return Err(AddDynamicChildError::NumberedHandleNotInSingleRunCollection);
        }

        if !collection_decl.allow_long_names && child_decl.name.len() > cm_types::MAX_NAME_LENGTH {
            return Err(AddDynamicChildError::NameTooLong { max_len: cm_types::MAX_NAME_LENGTH });
        }

        let mut dynamic_offers = child_args.dynamic_offers.unwrap_or_else(Vec::new);
        if dynamic_offers.len() > 0
            && collection_decl.allowed_offers != cm_types::AllowedOffers::StaticAndDynamic
        {
            return Err(AddDynamicChildError::DynamicOffersNotAllowed { collection_name });
        }

        let dynamic_capabilities = {
            let configs = child_args.config_capabilities.unwrap_or_else(Vec::new);
            if !configs.is_empty()
                && collection_decl.allowed_offers != cm_types::AllowedOffers::StaticAndDynamic
            {
                return Err(AddDynamicChildError::DynamicOffersNotAllowed { collection_name });
            }
            let mut dynamic_capabilities = Vec::new();
            for mut config in configs {
                let original_name = config.name.clone();
                if let Some(original_name) = original_name.as_ref() {
                    config.name =
                        Some(format!("{}.{}.{}", original_name, collection_name, child_decl.name));
                }

                dynamic_offers.push(fdecl::Offer::Config(fdecl::OfferConfiguration {
                    source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
                    source_name: config.name.clone(),
                    target_name: original_name,
                    ..Default::default()
                }));
                dynamic_capabilities.push(fdecl::Capability::Config(config));
            }
            dynamic_capabilities
        };

        let child_input = state
            .collection_inputs
            .get(&Name::new(&collection_name).unwrap())
            .expect("dict missing for declared collection")
            .shallow_copy();

        // Merge `ChildArgs.dictionary` entries into the child sandbox.
        if let Some(dictionary_client_end) = child_args.dictionary {
            let fidl_capability = fsandbox::Capability::Dictionary(dictionary_client_end);
            let any: Capability =
                fidl_capability.try_into().map_err(|_| AddDynamicChildError::InvalidDictionary)?;
            let dict = match any {
                Capability::Dictionary(d) => d,
                _ => return Err(AddDynamicChildError::InvalidDictionary),
            };
            let dict_entries = {
                let mut entries = dict.lock_entries();
                std::mem::replace(&mut *entries, std::collections::BTreeMap::new())
            };
            let mut child_dict_entries = child_input.capabilities.lock_entries();
            for (key, value) in dict_entries.into_iter() {
                // The child/collection Dict normally contains Routers created by component manager.
                // ChildArgs.dict may contain capabilities created by an external client.
                //
                // Currently there is no way to create a Rotuer externally, so assume these
                // are Open capabilities and convert them to Router here.
                //
                // TODO(https://fxbug.dev/319542502): Consider using the external Router type, once
                // it exists
                let router = match value {
                    Capability::Open(o) => Router::from_capability(o.into()),
                    _ => return Err(AddDynamicChildError::InvalidDictionary),
                };

                if child_dict_entries
                    .insert(key.clone(), Capability::Router(Box::new(router)))
                    .is_some()
                {
                    return Err(AddDynamicChildError::StaticRouteConflict { capability_name: key });
                }
            }
        }

        let (child, discover_fut) = state
            .add_child(
                self,
                child_decl,
                Some(&collection_decl),
                Some(dynamic_offers),
                Some(dynamic_capabilities),
                child_args.controller,
                child_input,
            )
            .await?;

        // Release the component state lock so DiscoverAction can acquire it.
        drop(state);

        // Wait for the Discover action to finish.
        discover_fut.await?;

        if let Some(start_reason) = maybe_start_reason {
            child
                .start(
                    &start_reason,
                    None,
                    IncomingCapabilities {
                        numbered_handles,
                        additional_namespace_entries: vec![],
                        dict: None,
                    },
                )
                .await
                .map_err(|err| {
                    debug!(%err, moniker=%child.moniker, "failed to start component instance");
                    AddDynamicChildError::ActionError { err }
                })?;
        }

        Ok(())
    }

    /// Removes the dynamic child, returning a future that will execute the
    /// destroy action.
    pub async fn remove_dynamic_child(
        self: &Arc<Self>,
        child_moniker: &ChildName,
    ) -> Result<(), ActionError> {
        let incarnation = {
            let state = self.lock_state().await;
            let state = match *state {
                InstanceState::Resolved(ref s) => s,
                _ => {
                    return Err(DestroyActionError::InstanceNotResolved {
                        moniker: self.moniker.clone(),
                    }
                    .into())
                }
            };
            if let Some(c) = state.get_child(&child_moniker) {
                c.incarnation_id()
            } else {
                let moniker = self.moniker.child(child_moniker.clone());
                return Err(DestroyActionError::InstanceNotFound { moniker }.into());
            }
        };
        self.destroy_child(child_moniker.clone(), incarnation).await
    }

    /// Stops this component.
    #[cfg(test)]
    pub async fn stop(self: &Arc<Self>) -> Result<(), ActionError> {
        ActionSet::register(self.clone(), StopAction::new(false)).await
    }

    /// Shuts down this component. This means the component and its subrealm are stopped and never
    /// allowed to restart again.
    pub async fn shutdown(
        self: &Arc<Self>,
        shutdown_type: ShutdownType,
    ) -> Result<(), ActionError> {
        ActionSet::register(self.clone(), ShutdownAction::new(shutdown_type)).await
    }

    /// Performs the stop protocol for this component instance. `shut_down` determines whether the
    /// instance is to be put in the shutdown state; see documentation on [ExecutionState].
    ///
    /// Clients should not call this function directly, except for `StopAction` and
    /// `ShutdownAction`.
    ///
    /// TODO(https://fxbug.dev/42067346): Limit the clients that call this directly.
    ///
    /// REQUIRES: All dependents have already been stopped.
    pub async fn stop_instance_internal(
        self: &Arc<Self>,
        shut_down: bool,
    ) -> Result<(), StopActionError> {
        let mut runtime = {
            let mut execution = self.lock_execution().await;
            let shut_down = execution.shut_down | shut_down;
            execution.shut_down = shut_down;
            execution.runtime.take()
        };

        let stop_result = {
            if let Some(ref mut runtime) = runtime {
                let stop_timer = Box::pin(async move {
                    let timer = fasync::Timer::new(fasync::Time::after(zx::Duration::from(
                        self.environment.stop_timeout(),
                    )));
                    timer.await;
                });
                let kill_timer = Box::pin(async move {
                    let timer = fasync::Timer::new(fasync::Time::after(zx::Duration::from(
                        DEFAULT_KILL_TIMEOUT,
                    )));
                    timer.await;
                });
                let ret = runtime
                    .stop_program(stop_timer, kill_timer)
                    .await
                    .map_err(StopActionError::ProgramStopError)?;
                if ret.request == StopRequestSuccess::KilledAfterTimeout
                    || ret.request == StopRequestSuccess::Killed
                {
                    warn!(
                        "component {} did not stop in {:?}. Killed it.",
                        self.moniker,
                        self.environment.stop_timeout()
                    );
                }
                if !shut_down && self.on_terminate == fdecl::OnTerminate::Reboot {
                    warn!(
                        "Component with on_terminate=REBOOT terminated: {}. \
                            Rebooting the system",
                        self.moniker
                    );
                    let top_instance = self
                        .top_instance()
                        .await
                        .map_err(|_| StopActionError::GetTopInstanceFailed)?;
                    top_instance.trigger_reboot().await;
                }

                if let Some(execution_controller_task) = runtime.execution_controller_task.as_mut()
                {
                    execution_controller_task.set_stop_status(ret.component_exit_status);
                }
                Some(ret.component_exit_status)
            } else {
                None
            }
        };

        // TODO(b/322564390): Move program_input_dict_additions into `ExecutionState` to avoid locking InstanceState.
        {
            let mut state = self.lock_state().await;
            if let InstanceState::Resolved(resolved_state) = &mut *state {
                resolved_state.program_input_dict_additions = None;
            };
        }

        // When the component is stopped, any child instances in collections must be destroyed.
        self.destroy_dynamic_children()
            .await
            .map_err(|err| StopActionError::DestroyDynamicChildrenFailed { err: Box::new(err) })?;
        if let Some(stop_result) = stop_result {
            let event = Event::new(self, EventPayload::Stopped { status: stop_result });
            self.hooks.dispatch(&event).await;
        }
        if let ExtendedInstance::Component(parent) =
            self.try_get_parent().map_err(|_| StopActionError::GetParentFailed)?
        {
            parent
                .destroy_child_if_single_run(
                    self.child_moniker().expect("child is root instance?"),
                    self.incarnation_id(),
                )
                .await;
        }
        Ok(())
    }

    async fn destroy_child_if_single_run(
        self: &Arc<Self>,
        child_moniker: &ChildName,
        incarnation: IncarnationId,
    ) {
        let single_run_colls = {
            let state = self.lock_state().await;
            let state = match *state {
                InstanceState::Resolved(ref s) => s,
                _ => {
                    // Component instance was not resolved, so no dynamic children.
                    return;
                }
            };
            let single_run_colls: HashSet<_> = state
                .decl()
                .collections
                .iter()
                .filter_map(|c| match c.durability {
                    fdecl::Durability::SingleRun => Some(c.name.clone()),
                    fdecl::Durability::Transient => None,
                })
                .collect();
            single_run_colls
        };
        if let Some(coll) = child_moniker.collection() {
            if single_run_colls.contains(coll) {
                let self_clone = self.clone();
                let child_moniker = child_moniker.clone();
                fasync::Task::spawn(async move {
                    if let Err(error) =
                        self_clone.destroy_child(child_moniker.clone(), incarnation).await
                    {
                        let moniker = self_clone.moniker.child(child_moniker);
                        warn!(
                            %moniker,
                            %error,
                            "single-run component could not be destroyed",
                        );
                    }
                })
                .detach();
            }
        }
    }

    /// Destroys this component instance.
    /// REQUIRES: All children have already been destroyed.
    pub async fn destroy_instance(self: &Arc<Self>) -> Result<(), DestroyActionError> {
        if self.persistent_storage {
            return Ok(());
        }
        // Clean up isolated storage.
        let uses = {
            let state = self.lock_state().await;
            match *state {
                InstanceState::Resolved(ref s) => s.resolved_component.decl.uses.clone(),
                _ => {
                    // The instance was never resolved and therefore never ran, it can't possibly
                    // have storage to clean up.
                    return Ok(());
                }
            }
        };
        for use_ in uses {
            if let UseDecl::Storage(use_storage) = use_ {
                match routing::route_and_delete_storage(use_storage.clone(), &self).await {
                    Ok(()) => (),
                    Err(ModelError::RoutingError { .. }) => {
                        // If the routing for this storage capability is invalid then there's no
                        // storage for us to delete. Ignore this error, and proceed.
                    }
                    Err(error) => {
                        // We received an error we weren't expecting, but we still want to destroy
                        // this instance. It's bad to leave storage state undeleted, but it would
                        // be worse to not continue with destroying this instance. Log the error,
                        // and proceed.
                        warn!(
                            component=%self.moniker, %error,
                            "failed to delete storage during instance destruction, proceeding with destruction anyway",
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Registers actions to destroy all dynamic children of collections belonging to this instance.
    async fn destroy_dynamic_children(self: &Arc<Self>) -> Result<(), ActionError> {
        let moniker_incarnations: Vec<_> = {
            let state = self.lock_state().await;
            let state = match *state {
                InstanceState::Resolved(ref s) => s,
                _ => {
                    // Component instance was not resolved, so no dynamic children.
                    return Ok(());
                }
            };
            state.children().map(|(k, c)| (k.clone(), c.incarnation_id())).collect()
        };
        let mut futures = vec![];
        // Destroy all children that belong to a collection.
        for (m, id) in moniker_incarnations {
            if m.collection().is_some() {
                let nf = self.destroy_child(m, id);
                futures.push(nf);
            }
        }
        join_all(futures).await.into_iter().fold(Ok(()), |acc, r| acc.and_then(|_| r))
    }

    pub async fn destroy_child(
        self: &Arc<Self>,
        moniker: ChildName,
        incarnation: IncarnationId,
    ) -> Result<(), ActionError> {
        // The child may not exist or may already be deleted by a previous DeleteChild action.
        let child = {
            let state = self.lock_state().await;
            match *state {
                InstanceState::Resolved(ref s) => {
                    let child = s.get_child(&moniker).map(|r| r.clone());
                    child
                }
                InstanceState::Destroyed => None,
                InstanceState::New | InstanceState::Unresolved(_) => {
                    panic!("DestroyChild: target is not resolved");
                }
            }
        };

        let Some(child) = child else { return Ok(()) };

        if child.incarnation_id() != incarnation {
            // The instance of the child we pulled from our live children does not match the
            // instance of the child we were asked to delete. This is possible if destroy_child
            // was called twice for the same child, and after the first call a child with the
            // same name was recreated.
            //
            // If there's already a live child with a different instance than what we were
            // asked to destroy, then surely the instance we wanted to destroy is long gone,
            // and we can safely return without doing any work.
            return Ok(());
        }

        // Wait for the child component to be destroyed
        ActionSet::register(child.clone(), DestroyAction::new()).await
    }

    /// Opens an object referenced by `path` from the outgoing directory of the component.
    /// The component must have a program and must be started, or this method will fail.
    pub async fn open_outgoing(
        &self,
        flags: fio::OpenFlags,
        path: &str,
        server_chan: &mut zx::Channel,
    ) -> Result<(), OpenOutgoingDirError> {
        let execution = self.lock_execution().await;
        let runtime = execution.runtime.as_ref().ok_or(OpenOutgoingDirError::InstanceNotRunning)?;
        let out_dir = runtime.outgoing_dir().ok_or(OpenOutgoingDirError::InstanceNonExecutable)?;
        let path = fuchsia_fs::canonicalize_path(&path);
        let server_chan = channel::take_channel(server_chan);
        let server_end = ServerEnd::new(server_chan);
        out_dir.open(flags, fio::ModeType::empty(), path, server_end)?;
        Ok(())
    }

    /// Returns an [`Open`] representation of the outgoing directory of the component. It performs
    /// the same checks as `open_outgoing`, but errors are surfaced at the server endpoint.
    pub fn get_outgoing(self: &Arc<Self>) -> Open {
        let weak_component = WeakComponentInstance::from(self);
        Open::new(
            move |scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  path: vfs::path::Path,
                  mut server_end: zx::Channel| {
                let component = match weak_component.upgrade() {
                    Ok(component) => component,
                    Err(err) => {
                        let _ = server_end.close_with_epitaph(err.as_zx_status());
                        return;
                    }
                };
                scope.spawn(async move {
                    match component.open_outgoing(flags, path.as_ref(), &mut server_end).await {
                        Ok(()) => {}
                        Err(err) => {
                            let _ = server_end.close_with_epitaph(err.as_zx_status());
                        }
                    }
                });
            },
            fio::DirentType::Directory,
        )
    }

    /// Connects `server_chan` to this instance's exposed directory if it has
    /// been resolved. Component must be resolved or destroyed before using
    /// this function, otherwise it will panic.
    pub async fn open_exposed(
        &self,
        server_chan: &mut zx::Channel,
    ) -> Result<(), OpenExposedDirError> {
        let state = self.lock_state().await;
        match &*state {
            InstanceState::Resolved(resolved_instance_state) => {
                // TODO(https://fxbug.dev/42161419): open_exposed does not have a rights input parameter, so
                // this makes use of the POSIX_[WRITABLE|EXECUTABLE] flags to open a connection
                // with those rights if available from the parent directory connection but without
                // failing if not available.
                let flags = fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::POSIX_WRITABLE
                    | fio::OpenFlags::POSIX_EXECUTABLE
                    | fio::OpenFlags::DIRECTORY;
                let server_chan = channel::take_channel(server_chan);
                let server_end = ServerEnd::new(server_chan);
                resolved_instance_state.open_exposed_dir(flags, Path::dot(), server_end).await;
                Ok(())
            }
            InstanceState::Destroyed => Err(OpenExposedDirError::InstanceDestroyed),
            _ => {
                panic!("Component must be resolved or destroyed before using this function")
            }
        }
    }

    /// Binds to the component instance in this instance, starting it if it's not already running.
    pub async fn start(
        self: &Arc<Self>,
        reason: &StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        incoming: IncomingCapabilities,
    ) -> Result<(), ActionError> {
        // Skip starting a component instance that was already started. It's important to bail out
        // here so we don't waste time starting eager children more than once.
        {
            let state = self.lock_state().await;
            let execution = self.lock_execution().await;
            if let Some(res) = start::should_return_early(&state, &execution, &self.moniker) {
                return res.map_err(Into::into);
            }
        }
        ActionSet::register(
            self.clone(),
            StartAction::new(reason.clone(), execution_controller_task, incoming),
        )
        .await?;

        let eager_children: Vec<_> = {
            let state = self.lock_state().await;
            match *state {
                InstanceState::Resolved(ref s) => s
                    .children()
                    .filter_map(|(_, r)| match r.startup {
                        fdecl::StartupMode::Eager => Some(r.clone()),
                        fdecl::StartupMode::Lazy => None,
                    })
                    .collect(),
                InstanceState::Destroyed => {
                    return Err(StartActionError::InstanceDestroyed {
                        moniker: self.moniker.clone(),
                    }
                    .into());
                }
                InstanceState::New | InstanceState::Unresolved(_) => {
                    panic!("start: not resolved")
                }
            }
        };
        Self::start_eager_children_recursive(eager_children).await.or_else(|e| match e {
            ActionError::StartError { err: StartActionError::InstanceShutDown { .. } } => Ok(()),
            _ => Err(StartActionError::EagerStartError {
                moniker: self.moniker.clone(),
                err: Box::new(e),
            }),
        })?;
        Ok(())
    }

    /// Starts a list of instances, and any eager children they may return.
    // This function recursively calls `start`, so it returns a BoxFuture,
    fn start_eager_children_recursive<'a>(
        instances_to_bind: Vec<Arc<ComponentInstance>>,
    ) -> BoxFuture<'a, Result<(), ActionError>> {
        let f = async move {
            let futures: Vec<_> = instances_to_bind
                .iter()
                .map(|component| async move { component.ensure_started(&StartReason::Eager).await })
                .collect();
            join_all(futures).await.into_iter().fold(Ok(()), |acc, r| acc.and_then(|_| r))?;
            Ok(())
        };
        Box::pin(f)
    }

    pub fn incarnation_id(&self) -> IncarnationId {
        match self.instanced_moniker().leaf() {
            Some(m) => m.instance(),
            // Assign 0 to the root component instance
            None => 0,
        }
    }

    pub fn instance_id(&self) -> Option<&InstanceId> {
        self.context.component_id_index().id_for_moniker(&self.moniker)
    }

    /// Runs the provided closure with this component's logger (if any) set as the default
    /// tracing subscriber for the duration of the closure.
    ///
    /// If the component is not running or does not have a logger, the tracing subscriber
    /// is unchanged, so logs will be attributed to component_manager.
    pub async fn with_logger_as_default<T>(&self, op: impl FnOnce() -> T) -> T {
        let execution = self.lock_execution().await;
        match &execution.runtime {
            Some(ComponentRuntime { logger: Some(ref logger), .. }) => {
                let logger = logger.clone() as Arc<dyn tracing::Subscriber + Send + Sync>;
                tracing::subscriber::with_default(logger, op)
            }
            _ => op(),
        }
    }

    /// Scoped this server_end to the component instance's Runtime. For the duration
    /// of the component's lifetime, when it's running, this channel will be
    /// kept alive.
    pub async fn scope_to_runtime(self: &Arc<Self>, server_end: zx::Channel) {
        let mut execution = self.lock_execution().await;
        execution.scope_server_end(server_end);
    }

    /// Returns the top instance (component manager's instance) by traversing parent links.
    async fn top_instance(self: &Arc<Self>) -> Result<Arc<ComponentManagerInstance>, ModelError> {
        let mut current = self.clone();
        loop {
            match current.try_get_parent()? {
                ExtendedInstance::Component(parent) => {
                    current = parent.clone();
                }
                ExtendedInstance::AboveRoot(parent) => {
                    return Ok(parent);
                }
            }
        }
    }

    /// Returns the effective persistent storage setting for a child.
    /// If the CollectionDecl exists and the `persistent_storage` field is set, return the setting.
    /// Otherwise, if the CollectionDecl or its `persistent_storage` field is not set, return
    /// `self.persistent_storage` as a default value for the child to inherit.
    fn persistent_storage_for_child(&self, collection: Option<&CollectionDecl>) -> bool {
        let default_persistent_storage = self.persistent_storage;
        if let Some(collection) = collection {
            collection.persistent_storage.unwrap_or(default_persistent_storage)
        } else {
            default_persistent_storage
        }
    }

    /// Looks up a component by moniker.
    ///
    /// The component instance in the component will be resolved if that has not already happened.
    pub async fn find_and_maybe_resolve(
        self: &Arc<Self>,
        look_up_moniker: &Moniker,
    ) -> Result<Arc<ComponentInstance>, ModelError> {
        let mut cur = self.clone();
        for moniker in look_up_moniker.path().iter() {
            cur = {
                let cur_state = cur.lock_resolved_state().await?;
                if let Some(c) = cur_state.get_child(moniker) {
                    c.clone()
                } else {
                    return Err(ModelError::instance_not_found(look_up_moniker.clone()));
                }
            };
        }
        cur.lock_resolved_state().await?;
        Ok(cur)
    }

    /// Finds a component matching the moniker, if such a component exists.
    /// This function has no side-effects.
    pub async fn find(
        self: &Arc<Self>,
        look_up_moniker: &Moniker,
    ) -> Option<Arc<ComponentInstance>> {
        let mut cur = self.clone();
        for moniker in look_up_moniker.path().iter() {
            cur = {
                let state = cur.lock_state().await;
                match &*state {
                    InstanceState::Resolved(r) => {
                        if let Some(c) = r.get_child(moniker) {
                            c.clone()
                        } else {
                            return None;
                        }
                    }
                    _ => return None,
                }
            };
        }
        Some(cur)
    }

    /// Finds a resolved component matching the moniker, if such a component exists.
    /// This function has no side-effects.
    #[cfg(test)]
    pub async fn find_resolved(
        self: &Arc<Self>,
        find_moniker: &Moniker,
    ) -> Option<Arc<ComponentInstance>> {
        let mut cur = self.clone();
        for moniker in find_moniker.path().iter() {
            cur = {
                let state = cur.lock_state().await;
                match &*state {
                    InstanceState::Resolved(r) => match r.get_child(moniker) {
                        Some(c) => c.clone(),
                        _ => return None,
                    },
                    _ => return None,
                }
            };
        }
        // Found the moniker, the last child in the chain of resolved parents. Is it resolved?
        let state = cur.lock_state().await;
        match &*state {
            InstanceState::Resolved(_) => Some(cur.clone()),
            _ => None,
        }
    }

    /// Starts the component instance in the given component if it's not already running.
    /// Returns the component that was bound to.
    #[cfg(test)]
    pub async fn start_instance<'a>(
        self: &Arc<Self>,
        moniker: &'a Moniker,
        reason: &StartReason,
    ) -> Result<Arc<ComponentInstance>, ModelError> {
        let component = self.find_and_maybe_resolve(moniker).await?;
        component.start(reason, None, IncomingCapabilities::default()).await?;
        Ok(component)
    }
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

#[async_trait]
impl ComponentInstanceInterface for ComponentInstance {
    type TopInstance = ComponentManagerInstance;

    fn instanced_moniker(&self) -> &InstancedMoniker {
        &self.instanced_moniker
    }

    fn moniker(&self) -> &Moniker {
        &self.moniker
    }

    fn child_moniker(&self) -> Option<&ChildName> {
        self.moniker.leaf()
    }

    fn url(&self) -> &str {
        &self.component_url
    }

    fn environment(&self) -> &dyn EnvironmentInterface<Self> {
        self.environment.as_ref()
    }

    fn policy_checker(&self) -> &GlobalPolicyChecker {
        &self.context.policy()
    }

    fn component_id_index(&self) -> &component_id_index::Index {
        self.context.component_id_index()
    }

    fn config_parent_overrides(&self) -> Option<&Vec<cm_rust::ConfigOverride>> {
        self.config_parent_overrides.as_ref()
    }

    fn try_get_parent(&self) -> Result<ExtendedInstance, ComponentInstanceError> {
        self.parent.upgrade()
    }

    async fn lock_resolved_state<'a>(
        self: &'a Arc<Self>,
    ) -> Result<Box<dyn ResolvedInstanceInterface<Component = Self> + 'a>, ComponentInstanceError>
    {
        Ok(Box::new(ComponentInstance::lock_resolved_state(self).await.map_err(|err| {
            let err: anyhow::Error = err.into();
            ComponentInstanceError::ResolveFailed { moniker: self.moniker.clone(), err: err.into() }
        })?))
    }
}

impl std::fmt::Debug for ComponentInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentInstance")
            .field("component_url", &self.component_url)
            .field("startup", &self.startup)
            .field("moniker", &self.instanced_moniker)
            .finish()
    }
}

/// The execution state of a component.
pub struct ExecutionState {
    /// True if the component instance has shut down. This means that the component is stopped
    /// and cannot be restarted.
    shut_down: bool,

    /// Runtime support for the component. From component manager's point of view, the component
    /// instance is running iff this field is set.
    pub runtime: Option<ComponentRuntime>,
}

impl ExecutionState {
    /// Creates a new ExecutionState.
    pub fn new() -> Self {
        Self { shut_down: false, runtime: None }
    }

    /// Returns whether the component is started, i.e. if it has a runtime.
    pub fn is_started(&self) -> bool {
        self.runtime.is_some()
    }

    /// Returns whether the instance has shut down.
    pub fn is_shut_down(&self) -> bool {
        self.shut_down
    }

    /// Enables the component to restart after being shut down. Used by the UnresolveAction.
    /// Use of this function is strongly discouraged.
    pub fn reset_shut_down(&mut self) {
        self.shut_down = false;
    }

    /// Scope server_end to `runtime` of this state. This ensures that the channel
    /// will be kept alive as long as runtime is set to Some(...). If it is
    /// None when this method is called, this operation is a no-op and the channel
    /// will be dropped.
    pub fn scope_server_end(&mut self, server_end: zx::Channel) {
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.add_scoped_server_end(server_end);
        }
    }
}

/// The mutable state of a component instance.
pub enum InstanceState {
    /// The instance was just created.
    New,
    /// A Discovered event has been dispatched for the instance, but it has not been resolved yet.
    Unresolved(UnresolvedInstanceState),
    /// The instance has been resolved.
    Resolved(ResolvedInstanceState),
    /// The instance has been destroyed. It has no content and no further actions may be registered
    /// on it.
    Destroyed,
}

impl InstanceState {
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
            InstanceState::Unresolved(unresolved) => {
                Some(unresolved.instance_token(moniker, context))
            }
            InstanceState::Resolved(resolved) => Some(resolved.instance_token(moniker, context)),
            InstanceState::Destroyed => None,
        }
    }
}

impl fmt::Debug for InstanceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::New => "New",
            Self::Unresolved(_) => "Discovered",
            Self::Resolved(_) => "Resolved",
            Self::Destroyed => "Destroyed",
        };
        f.write_str(s)
    }
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
        (
            std::mem::replace(&mut self.instance_token_state, Default::default()),
            self.component_input.clone(),
        )
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

    /// The ExecutionScope for this component. Pseudo directories should be hosted with this
    /// scope to tie their life-time to that of the component.
    execution_scope: ExecutionScope,

    /// Result of resolving the component.
    pub resolved_component: Component,

    /// All child instances, indexed by child moniker.
    children: HashMap<ChildName, Arc<ComponentInstance>>,

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
    program_output_dict: Dict,

    /// Dictionary of extra capabilities passed to the component when it is started.
    // TODO(b/322564390): Move this into `ExecutionState` once stop action releases lock on it
    pub program_input_dict_additions: Option<Dict>,

    /// Dicts containing the capabilities we want to provide to each collection. Each new
    /// dynamic child gets a clone of one of these inputs (which is potentially extended by
    /// dynamic offers).
    collection_inputs: HashMap<Name, ComponentInput>,

    /// The environments declared by this component.
    bedrock_environments: HashMap<Name, ComponentEnvironment>,
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
        let execution_scope = ExecutionScope::new();

        let environments = Self::instantiate_environments(component, &resolved_component.decl);
        let decl = resolved_component.decl.clone();

        // Perform the policy check for debug capabilities now, instead of during routing. All the info we
        // need to perform this check is already available to us. This way, we don't have to propagate this
        // info to sandbox capabilities just so they can enforce the policy.
        for (env_name, env) in &environments {
            for capability_name in env.debug_registry().debug_capabilities.keys() {
                let parent = match env.parent() {
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

        let mut state = Self {
            weak_component,
            instance_token_state,
            execution_scope,
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
            collection_inputs: HashMap::new(),
            bedrock_environments: HashMap::new(),
        };
        state.add_static_children(component).await?;

        let mut child_inputs = HashMap::new();
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

    /// Returns a router that starts the component upon a capability request,
    /// then delegates the request to the program outgoing dict of the component.
    pub fn start_component_on_request(
        component: &Arc<ComponentInstance>,
        decl: &ComponentDecl,
        capability_name: Name,
    ) -> Router {
        if decl.program.is_none() {
            return Router::new_error(OpenOutgoingDirError::InstanceNonExecutable.into());
        }
        let outgoing_dict = Self::build_program_outgoing_dict(component, &decl.capabilities);
        let weak_component = WeakComponentInstance::new(component);
        let route_fn = move |request: Request| {
            let weak_component = weak_component.clone();
            let outgoing_dict = outgoing_dict.clone();
            let capability_name = capability_name.clone();
            let target_moniker = request.target.moniker.clone();
            async move {
                // If the component is already started, this will be a no-op.
                weak_component
                    .ensure_started(&StartReason::AccessCapability {
                        target: target_moniker,
                        name: capability_name.clone(),
                    })
                    .await?;
                let cap = outgoing_dict.get_capability(iter::once(capability_name.as_str()));
                match cap {
                    Some(cap) => cap.route(request).await,
                    None => Err(RoutingError::BedrockNotPresentInDictionary {
                        name: capability_name.into(),
                    }
                    .into()),
                }
            }
            .boxed()
        };
        Router::new(route_fn)
    }

    /// Builds the program outgoing dict such that most work is done once and cached even if
    /// the underlying outgoing directory FIDL endpoint changes across restarts.
    ///
    /// The program outgoing dict is a dict of routers. For each capability declared by a
    /// program that is published at some path in the outgoing directory, there will be a
    /// [`Router`] that requests that capability, keyed by capability name.
    fn build_program_outgoing_dict(
        component: &Arc<ComponentInstance>,
        capabilities: &Vec<CapabilityDecl>,
    ) -> Dict {
        let outgoing_dir = component.get_outgoing();
        let weak_component = WeakComponentInstance::from(component);
        let dict = Dict::new();
        for capability in capabilities {
            let Some(path) = capability.path() else {
                continue;
            };
            let name = capability.name().as_str();
            let path = fuchsia_fs::canonicalize_path(path.as_str());
            let open = outgoing_dir.clone().downscope_path(sandbox::Path::new(path));
            let router = Capability::Open(open)
                .with_capability_requested_hook(weak_component.clone(), capability.name().clone());
            dict.insert_capability(iter::once(name), router.into());
        }
        dict
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

    /// This component's `ExecutionScope`.
    /// Pseudo-directories can be opened with this scope to tie their lifetime
    /// to this component.
    pub fn execution_scope(&self) -> &ExecutionScope {
        &self.execution_scope
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
                self.execution_scope.clone(),
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
        // Unwrap is safe because `ResolvedInstanceState` is held within `ComponentInstance`.
        let routing_task_group = self.weak_component.upgrade().unwrap().blocking_task_group();
        let dict = Router::dict_routers_to_open(
            &self.weak_component,
            &self.component_output_dict,
            routing_task_group,
        );
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
            let routing_fn = route_fn(component.clone(), request);
            let dirent_type = match type_name {
                CapabilityTypeName::Directory => fio::DirentType::Directory,
                CapabilityTypeName::EventStream => fio::DirentType::Service,
                CapabilityTypeName::Protocol => fio::DirentType::Service,
                CapabilityTypeName::Service => fio::DirentType::Directory,
                CapabilityTypeName::Storage => fio::DirentType::Directory,
                CapabilityTypeName::Dictionary => fio::DirentType::Service,
                // The below don't appear in exposed or used dir
                CapabilityTypeName::Resolver
                | CapabilityTypeName::Runner
                | CapabilityTypeName::Config => fio::DirentType::Unknown,
            };
            let open = Open::new(routing_fn, dirent_type);
            target_dict.insert_capability(iter::once(target_name), open.into());
        }
    }

    /// Opens the exposed directory bound to this instance.
    pub async fn open_exposed_dir(
        &self,
        flags: fio::OpenFlags,
        path: vfs::path::Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        self.get_exposed_dir().await.open(
            self.execution_scope.clone(),
            flags,
            path,
            server_end.into_channel(),
        );
    }

    async fn get_exposed_dir(&self) -> &Open {
        let create_exposed_dir = async {
            let exposed_dict = self.make_exposed_dict().await;
            exposed_dict
                .try_into_open()
                .expect("converting exposed dict to open should always succeed")
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
    async fn add_child(
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
            ComponentInput::empty(),
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
                &self.program_output_dict,
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
        );
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

    fn validate_and_convert_dynamic_component(
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

    async fn discover_static_children(&self, mut child_inputs: HashMap<Name, ComponentInput>) {
        for (child_name, child_instance) in &self.children {
            let child_name = Name::new(child_name.name()).unwrap();
            let child_input = child_inputs.remove(&child_name).expect("missing child dict");
            let _discover_fut = child_instance
                .clone()
                .lock_actions()
                .await
                .register_no_wait(&child_instance, DiscoverAction::new(child_input));
        }
    }
}

impl Drop for ResolvedInstanceState {
    fn drop(&mut self) {
        self.execution_scope.shutdown();
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

impl ProgramRuntime {
    pub fn new(program: Program, component: WeakComponentInstance) -> Self {
        let terminated_fut = program.on_terminate();
        let exit_listener = fasync::Task::spawn(async move {
            terminated_fut.await;
            if let Ok(component) = component.upgrade() {
                let mut actions = component.lock_actions().await;
                let stop_nf = actions.register_no_wait(&component, StopAction::new(false));
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
    ) -> Result<ComponentStopOutcome, program::StopError> {
        let stop_result = self.program.stop_or_kill_with_timeout(stop_timer, kill_timer).await;
        // Drop the program and join on the exit listener. Dropping the program
        // should cause the exit listener to stop waiting for the channel epitaph and
        // exit.
        //
        // Note: this is more reliable than just cancelling `exit_listener` because
        // even after cancellation future may still run for a short period of time
        // before getting dropped. If that happens there is a chance of scheduling a
        // duplicate Stop action.
        //
        // TODO(https://fxbug.dev/326626515): Start using `_escrow_request`.
        let _escrow_request = self.program.finalize();
        self.exit_listener.await;
        stop_result
    }
}

/// The execution state for a component instance that has started running.
///
/// If the component instance has a program, it may also have a [`ProgramRuntime`].
pub struct ComponentRuntime {
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
    execution_controller_task: Option<controller::ExecutionControllerTask>,

    /// Logger attributed to this component.
    ///
    /// Only set if the component uses the `fuchsia.logger.LogSink` protocol.
    logger: Option<Arc<ScopedLogger>>,
}

impl ComponentRuntime {
    pub fn new(
        start_reason: StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        logger: Option<ScopedLogger>,
    ) -> Self {
        let timestamp = zx::Time::get_monotonic();
        ComponentRuntime {
            program: None,
            timestamp,
            binder_server_ends: vec![],
            start_reason,
            execution_controller_task,
            logger: logger.map(Arc::new),
        }
    }

    /// If this component is associated with a running [Program], obtain a capability
    /// representing its outgoing directory.
    pub fn outgoing_dir(&self) -> Option<&fio::DirectoryProxy> {
        self.program.as_ref().map(|program_runtime| program_runtime.program.outgoing())
    }

    /// If this component is associated with a running [Program], obtain a capability
    /// representing its runtime directory.
    pub fn runtime_dir(&self) -> Option<&fio::DirectoryProxy> {
        self.program.as_ref().map(|program_runtime| program_runtime.program.runtime())
    }

    /// Associates the [ComponentRuntime] with a running [Program].
    ///
    /// Creates a background task waiting for the program to terminate. When that happens, use the
    /// [WeakComponentInstance] to stop the component.
    pub fn set_program(&mut self, program: Program, component: WeakComponentInstance) {
        self.program = Some(ProgramRuntime::new(program, component));
    }

    /// Stop the program, if any. The timer defines how long the runner is given to stop the
    /// program gracefully before we request the controller to terminate the program.
    ///
    /// Regardless if the runner honored our request, after this method, the [`ComponentRuntime`] is
    /// no longer associated with a [Program].
    pub async fn stop_program<'a, 'b>(
        &'a mut self,
        stop_timer: BoxFuture<'a, ()>,
        kill_timer: BoxFuture<'b, ()>,
    ) -> Result<ComponentStopOutcome, program::StopError> {
        let program = self.program.take();
        // Potentially there is no program, perhaps because the component
        // has no running code. In this case this is a no-op.
        if let Some(program) = program {
            program.stop(stop_timer, kill_timer).await
        } else {
            Ok(ComponentStopOutcome {
                request: StopRequestSuccess::NoController,
                component_exit_status: zx::Status::OK,
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

trait RouterExt: Routable + Clone + Send + Sync + 'static {
    /// Returns a router that delegates to the event system if the capability request is
    /// intercepted by some hook, and delegates to the current router otherwise.
    fn with_capability_requested_hook(self, source: WeakComponentInstance, name: Name) -> Router {
        let name = name.to_string();
        let route_fn = move |request: Request| {
            let source = source.clone();
            let name = name.clone();
            let router = self.clone();
            async move {
                let source = source.upgrade().map_err(RoutingError::from)?;
                let target = request.target.upgrade().map_err(RoutingError::from)?;
                let (receiver, sender) = CapabilityReceiver::new();
                let event = Event::new(
                    &target,
                    EventPayload::CapabilityRequested {
                        source_moniker: source.moniker.clone(),
                        name,
                        receiver: receiver.clone(),
                    },
                );
                source.hooks.dispatch(&event).await;
                if receiver.is_taken() {
                    Ok(sender.into())
                } else {
                    router.route(request).await
                }
            }
            .boxed()
        };
        Router::new(route_fn)
    }
}

impl<T: Routable + Clone + Send + Sync + 'static> RouterExt for T {}

#[cfg(test)]
pub mod tests {
    use {
        super::*,
        crate::model::{
            actions::test_utils::is_discovered,
            events::{registry::EventSubscription, stream::EventStream},
            hooks::EventType,
            testing::{
                mocks::ControllerActionResponse,
                out_dir::OutDir,
                routing_test_helpers::{RoutingTest, RoutingTestBuilder},
                test_helpers::{component_decl_with_test_runner, ActionsTest, ComponentInfo},
            },
        },
        assert_matches::assert_matches,
        cm_rust::{
            Availability, ChildRef, DependencyType, ExposeSource, OfferDecl, OfferDirectoryDecl,
            OfferProtocolDecl, OfferServiceDecl, OfferSource, OfferTarget, UseEventStreamDecl,
            UseSource,
        },
        cm_rust_testing::*,
        fasync::TestExecutor,
        fidl::endpoints::DiscoverableProtocolMarker,
        fidl_fuchsia_logger as flogger, fuchsia_async as fasync, fuchsia_zircon as zx,
        futures::{channel::mpsc, StreamExt, TryStreamExt},
        routing_test_helpers::component_id_index::make_index_file,
        std::{panic, task::Poll},
        tracing::info,
        vfs::service::host,
    };

    #[fuchsia::test]
    async fn started_event_timestamp_matches_component() {
        let test =
            RoutingTest::new("root", vec![("root", ComponentDeclBuilder::new().build())]).await;

        let mut event_source =
            test.builtin_environment.event_source_factory.create_for_above_root();
        let mut event_stream = event_source
            .subscribe(
                vec![
                    EventType::Discovered.into(),
                    EventType::Resolved.into(),
                    EventType::Started.into(),
                    EventType::DebugStarted.into(),
                ]
                .into_iter()
                .map(|event: Name| {
                    EventSubscription::new(UseEventStreamDecl {
                        source_name: event,
                        source: UseSource::Parent,
                        scope: None,
                        target_path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                        filter: None,
                        availability: Availability::Required,
                    })
                })
                .collect(),
            )
            .await
            .expect("subscribe to event stream");

        let root = test.model.root().clone();
        let (f, bind_handle) = async move {
            root.start_instance(&Moniker::root(), &StartReason::Root).await.expect("failed to bind")
        }
        .remote_handle();
        fasync::Task::spawn(f).detach();
        let discovered_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Discovered).await;
        let resolved_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Resolved).await;
        let started_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Started).await;
        let debug_started_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::DebugStarted).await;

        assert!(discovered_timestamp < resolved_timestamp);
        assert!(resolved_timestamp < started_timestamp);
        assert!(started_timestamp == debug_started_timestamp);

        let component = bind_handle.await;
        let component_timestamp =
            component.lock_execution().await.runtime.as_ref().unwrap().timestamp;
        assert_eq!(component_timestamp, started_timestamp);
    }

    #[fuchsia::test]
    /// Validate that if the ComponentController channel is closed that the
    /// the component is stopped.
    async fn test_early_component_exit() {
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("a").eager().build())
                    .build(),
            ),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager().build())
                    .build(),
            ),
            ("b", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;

        let mut event_source =
            test.builtin_environment.lock().await.event_source_factory.create_for_above_root();
        let mut stop_event_stream = event_source
            .subscribe(vec![EventSubscription::new(UseEventStreamDecl {
                source_name: EventType::Stopped.into(),
                source: UseSource::Parent,
                scope: None,
                target_path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                filter: None,
                availability: Availability::Required,
            })])
            .await
            .expect("couldn't susbscribe to event stream");

        let a_moniker: Moniker = vec!["a"].try_into().unwrap();
        let b_moniker: Moniker = vec!["a", "b"].try_into().unwrap();

        let component_b = test.look_up(b_moniker.clone()).await;

        // Start the root so it and its eager children start.
        let _root = test
            .model
            .root()
            .start_instance(&Moniker::root(), &StartReason::Root)
            .await
            .expect("failed to start root");
        test.runner
            .wait_for_urls(&["test:///root_resolved", "test:///a_resolved", "test:///b_resolved"])
            .await;

        // Check that the eager 'b' has started.
        assert!(component_b.is_started().await);

        let b_info = ComponentInfo::new(component_b.clone()).await;
        b_info.check_not_shut_down(&test.runner).await;

        // Tell the runner to close the controller channel
        test.runner.abort_controller(&b_info.channel_id);

        // Verify that we get a stop event as a result of the controller
        // channel close being observed.
        let stop_event = stop_event_stream
            .wait_until(EventType::Stopped, b_moniker.clone())
            .await
            .unwrap()
            .event;
        assert_eq!(stop_event.target_moniker, b_moniker.clone().into());

        // Verify that a parent of the exited component can still be stopped
        // properly.
        ActionSet::register(
            test.look_up(a_moniker.clone()).await,
            ShutdownAction::new(ShutdownType::Instance),
        )
        .await
        .expect("Couldn't trigger shutdown");
        // Check that we get a stop even which corresponds to the parent.
        let parent_stop = stop_event_stream
            .wait_until(EventType::Stopped, a_moniker.clone())
            .await
            .unwrap()
            .event;
        assert_eq!(parent_stop.target_moniker, a_moniker.clone().into());
    }

    #[fuchsia::test]
    async fn unresolve_test() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager().build())
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager().build())
                    .child(ChildBuilder::new().name("d").eager().build())
                    .build(),
            ),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;

        // Resolve each component.
        test.look_up(Moniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;

        // Just unresolve component a and children
        assert_matches!(component_a.unresolve().await, Ok(()));
        assert!(is_discovered(&component_a).await);
        assert!(is_discovered(&component_b).await);
        assert!(is_discovered(&component_c).await);
        assert!(is_discovered(&component_d).await);

        // Unresolve again, which is ok because UnresolveAction is idempotent.
        assert_matches!(component_a.unresolve().await, Ok(()));
        assert!(is_discovered(&component_a).await);
    }

    #[fuchsia::test]
    async fn realm_instance_id() {
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("a").eager().build())
                    .build(),
            ),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager().build())
                    .build(),
            ),
            ("b", component_decl_with_test_runner()),
        ];

        let instance_id = InstanceId::new_random(&mut rand::thread_rng());
        let index = {
            let mut index = component_id_index::Index::default();
            index.insert(Moniker::root(), instance_id.clone()).unwrap();
            index
        };
        let component_id_index_path = make_index_file(index).unwrap();
        let test = RoutingTestBuilder::new("root", components)
            .set_component_id_index_path(
                component_id_index_path.path().to_owned().try_into().unwrap(),
            )
            .build()
            .await;

        let root = test.model.root();
        let root_realm = root.start_instance(&Moniker::root(), &StartReason::Root).await.unwrap();
        assert_eq!(instance_id, *root_realm.instance_id().unwrap());

        let a_realm = root
            .start_instance(&Moniker::try_from(vec!["a"]).unwrap(), &StartReason::Root)
            .await
            .unwrap();
        assert_eq!(None, a_realm.instance_id());
    }

    async fn wait_until_event_get_timestamp(
        event_stream: &mut EventStream,
        event_type: EventType,
    ) -> zx::Time {
        event_stream.wait_until(event_type, Moniker::root()).await.unwrap().event.timestamp.clone()
    }

    #[fuchsia::test]
    async fn shutdown_component_interface_no_dynamic() {
        let example_offer = OfferDecl::Directory(OfferDirectoryDecl {
            source: OfferSource::static_child("a".to_string()),
            target: OfferTarget::static_child("b".to_string()),
            source_name: "foo".parse().unwrap(),
            source_dictionary: None,
            target_name: "foo".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            rights: None,
            subdir: None,
            availability: Availability::Required,
        });
        let example_capability = CapabilityBuilder::protocol().name("bar").build();
        let example_expose =
            ExposeBuilder::protocol().name("bar").source(ExposeSource::Self_).build();
        let example_use = UseBuilder::protocol().name("baz").build();
        let env_a = EnvironmentBuilder::new().name("env_a").build();
        let env_b = EnvironmentBuilder::new().name("env_b").build();

        let root_decl = ComponentDeclBuilder::new()
            .environment(env_a.clone())
            .environment(env_b.clone())
            .child(ChildBuilder::new().name("a").environment("env_a").build())
            .child(ChildBuilder::new().name("b").environment("env_b").build())
            .child_default("c")
            .collection_default("coll")
            .offer(example_offer.clone())
            .expose(example_expose.clone())
            .capability(example_capability.clone())
            .use_(example_use.clone())
            .build();
        let components = vec![
            ("root", root_decl.clone()),
            ("a", component_decl_with_test_runner()),
            ("b", component_decl_with_test_runner()),
            ("c", component_decl_with_test_runner()),
        ];

        let test = RoutingTestBuilder::new("root", components).build().await;

        let root_component =
            test.model.root().start_instance(&Moniker::root(), &StartReason::Root).await.unwrap();

        let root_resolved = root_component.lock_resolved_state().await.expect("resolve failed");

        assert_eq!(vec![example_capability], shutdown::Component::capabilities(&*root_resolved));
        assert_eq!(vec![example_use], shutdown::Component::uses(&*root_resolved));
        assert_eq!(vec![example_offer], shutdown::Component::offers(&*root_resolved));
        assert_eq!(vec![example_expose], shutdown::Component::exposes(&*root_resolved));
        assert_eq!(
            vec![root_decl.collections[0].clone()],
            shutdown::Component::collections(&*root_resolved)
        );
        assert_eq!(vec![env_a, env_b], shutdown::Component::environments(&*root_resolved));

        let mut children = shutdown::Component::children(&*root_resolved);
        children.sort();
        assert_eq!(
            vec![
                shutdown::Child {
                    moniker: "a".try_into().unwrap(),
                    environment_name: Some("env_a".to_string()),
                },
                shutdown::Child {
                    moniker: "b".try_into().unwrap(),
                    environment_name: Some("env_b".to_string()),
                },
                shutdown::Child { moniker: "c".try_into().unwrap(), environment_name: None },
            ],
            children
        );
    }

    #[fuchsia::test]
    async fn shutdown_component_interface_dynamic_children_and_offers() {
        let example_offer = OfferDecl::Directory(OfferDirectoryDecl {
            source: OfferSource::static_child("a".to_string()),
            target: OfferTarget::static_child("b".to_string()),
            source_name: "foo".parse().unwrap(),
            source_dictionary: None,
            target_name: "foo".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            rights: None,
            subdir: None,
            availability: Availability::Required,
        });

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .environment(EnvironmentBuilder::new().name("env_a"))
                    .environment(EnvironmentBuilder::new().name("env_b"))
                    .child(ChildBuilder::new().name("a").environment("env_a").build())
                    .child_default("b")
                    .collection(
                        CollectionBuilder::new()
                            .name("coll_1")
                            .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                    )
                    .collection(
                        CollectionBuilder::new()
                            .name("coll_2")
                            .environment("env_b")
                            .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                    )
                    .offer(example_offer.clone())
                    .build(),
            ),
            ("a", component_decl_with_test_runner()),
            ("b", component_decl_with_test_runner()),
        ];

        let test = ActionsTest::new("root", components, Some(Moniker::root())).await;

        test.create_dynamic_child("coll_1", "a").await;
        test.create_dynamic_child_with_args(
            "coll_1",
            "b",
            fcomponent::CreateChildArgs {
                dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                    source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                        name: "a".into(),
                        collection: Some("coll_1".parse().unwrap()),
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
        test.create_dynamic_child("coll_2", "a").await;

        let example_dynamic_offer = OfferDecl::Protocol(OfferProtocolDecl {
            source: OfferSource::Child(ChildRef {
                name: "a".into(),
                collection: Some("coll_1".parse().unwrap()),
            }),
            target: OfferTarget::Child(ChildRef {
                name: "b".into(),
                collection: Some("coll_1".parse().unwrap()),
            }),
            source_dictionary: None,
            source_name: "dyn_offer_source_name".parse().unwrap(),
            target_name: "dyn_offer_target_name".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        let root_component = test.look_up(Moniker::root()).await;

        {
            let root_resolved = root_component.lock_resolved_state().await.expect("resolving");

            let mut children = shutdown::Component::children(&*root_resolved);
            children.sort();
            pretty_assertions::assert_eq!(
                vec![
                    shutdown::Child {
                        moniker: "a".try_into().unwrap(),
                        environment_name: Some("env_a".to_string()),
                    },
                    shutdown::Child { moniker: "b".try_into().unwrap(), environment_name: None },
                    shutdown::Child {
                        moniker: "coll_1:a".try_into().unwrap(),
                        environment_name: None
                    },
                    shutdown::Child {
                        moniker: "coll_1:b".try_into().unwrap(),
                        environment_name: None
                    },
                    shutdown::Child {
                        moniker: "coll_2:a".try_into().unwrap(),
                        environment_name: Some("env_b".to_string()),
                    },
                ],
                children
            );
            pretty_assertions::assert_eq!(
                vec![example_offer.clone(), example_dynamic_offer.clone()],
                shutdown::Component::offers(&*root_resolved)
            )
        }

        // Destroy `coll_1:b`. It should not be listed. The dynamic offer should be deleted.
        root_component
            .destroy_child("coll_1:b".try_into().unwrap(), 2)
            .await
            .expect("destroy failed");

        {
            let root_resolved = root_component.lock_resolved_state().await.expect("resolving");

            let mut children = shutdown::Component::children(&*root_resolved);
            children.sort();
            pretty_assertions::assert_eq!(
                vec![
                    shutdown::Child {
                        moniker: "a".try_into().unwrap(),
                        environment_name: Some("env_a".to_string()),
                    },
                    shutdown::Child { moniker: "b".try_into().unwrap(), environment_name: None },
                    shutdown::Child {
                        moniker: "coll_1:a".try_into().unwrap(),
                        environment_name: None
                    },
                    shutdown::Child {
                        moniker: "coll_2:a".try_into().unwrap(),
                        environment_name: Some("env_b".to_string()),
                    },
                ],
                children
            );

            pretty_assertions::assert_eq!(
                vec![example_offer.clone()],
                shutdown::Component::offers(&*root_resolved)
            )
        }

        // Recreate `coll_1:b`, this time with a dynamic offer from `a` in the other
        // collection. Both versions should be listed.
        test.create_dynamic_child_with_args(
            "coll_1",
            "b",
            fcomponent::CreateChildArgs {
                dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                    source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                        name: "a".into(),
                        collection: Some("coll_2".parse().unwrap()),
                    })),
                    source_name: Some("dyn_offer2_source_name".to_string()),
                    target_name: Some("dyn_offer2_target_name".to_string()),
                    dependency_type: Some(fdecl::DependencyType::Strong),
                    ..Default::default()
                })]),
                ..Default::default()
            },
        )
        .await
        .expect("failed to create child");

        let example_dynamic_offer2 = OfferDecl::Protocol(OfferProtocolDecl {
            source: OfferSource::Child(ChildRef {
                name: "a".into(),
                collection: Some("coll_2".parse().unwrap()),
            }),
            target: OfferTarget::Child(ChildRef {
                name: "b".into(),
                collection: Some("coll_1".parse().unwrap()),
            }),
            source_name: "dyn_offer2_source_name".parse().unwrap(),
            source_dictionary: None,
            target_name: "dyn_offer2_target_name".parse().unwrap(),
            dependency_type: DependencyType::Strong,
            availability: Availability::Required,
        });

        {
            let root_resolved = root_component.lock_resolved_state().await.expect("resolving");

            let mut children = shutdown::Component::children(&*root_resolved);
            children.sort();
            pretty_assertions::assert_eq!(
                vec![
                    shutdown::Child {
                        moniker: "a".try_into().unwrap(),
                        environment_name: Some("env_a".to_string()),
                    },
                    shutdown::Child { moniker: "b".try_into().unwrap(), environment_name: None },
                    shutdown::Child {
                        moniker: "coll_1:a".try_into().unwrap(),
                        environment_name: None
                    },
                    shutdown::Child {
                        moniker: "coll_1:b".try_into().unwrap(),
                        environment_name: None
                    },
                    shutdown::Child {
                        moniker: "coll_2:a".try_into().unwrap(),
                        environment_name: Some("env_b".to_string()),
                    },
                ],
                children
            );

            pretty_assertions::assert_eq!(
                vec![example_offer.clone(), example_dynamic_offer2.clone()],
                shutdown::Component::offers(&*root_resolved)
            )
        }
    }

    // TODO(https://fxbug.dev/42066274)
    #[ignore]
    #[fuchsia::test]
    async fn creating_dynamic_child_with_offer_cycle_fails() {
        let example_offer = OfferDecl::Service(OfferServiceDecl {
            source: OfferSource::Collection("coll".parse().unwrap()),
            source_name: "foo".parse().unwrap(),
            source_dictionary: None,
            source_instance_filter: None,
            renamed_instances: None,
            target: OfferTarget::static_child("static_child".to_string()),
            target_name: "foo".parse().unwrap(),
            availability: Availability::Required,
        });

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .child_default("static_child")
                    .collection(
                        CollectionBuilder::new()
                            .name("coll")
                            .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                    )
                    .offer(example_offer.clone())
                    .build(),
            ),
            ("static_child", component_decl_with_test_runner()),
        ];

        let test = ActionsTest::new("root", components, Some(Moniker::root())).await;

        let res = test
            .create_dynamic_child_with_args(
                "coll",
                "dynamic_child",
                fcomponent::CreateChildArgs {
                    dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "static_child".into(),
                            collection: None,
                        })),
                        source_name: Some("bar".to_string()),
                        target_name: Some("bar".to_string()),
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
            )
            .await;
        assert_matches!(res, Err(fcomponent::Error::InvalidArguments));
    }

    // TODO(https://fxbug.dev/42066274)
    #[ignore]
    #[fuchsia::test]
    async fn creating_cycle_between_collections_fails() {
        let static_collection_offer = OfferDecl::Service(OfferServiceDecl {
            source: OfferSource::Collection("coll1".parse().unwrap()),
            source_name: "foo".parse().unwrap(),
            source_dictionary: None,
            source_instance_filter: None,
            renamed_instances: None,
            target: OfferTarget::Collection("coll2".parse().unwrap()),
            target_name: "foo".parse().unwrap(),
            availability: Availability::Required,
        });

        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .collection(
                    CollectionBuilder::new()
                        .name("coll1")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .collection(
                    CollectionBuilder::new()
                        .name("coll2")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .offer(static_collection_offer.clone())
                .build(),
        )];

        let test = ActionsTest::new("root", components, Some(Moniker::root())).await;
        test.create_dynamic_child("coll2", "dynamic_src").await;
        let cycle_res = test
            .create_dynamic_child_with_args(
                "coll1",
                "dynamic_sink",
                fcomponent::CreateChildArgs {
                    dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "dynamic_src".into(),
                            collection: Some("coll2".parse().unwrap()),
                        })),
                        source_name: Some("bar".to_string()),
                        target_name: Some("bar".to_string()),
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
            )
            .await;
        assert_matches!(cycle_res, Err(fcomponent::Error::InvalidArguments));
    }

    #[fuchsia::test]
    async fn creating_dynamic_child_with_offer_from_undefined_on_self_fails() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .collection(
                    CollectionBuilder::new()
                        .name("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .build(),
        )];

        let test = ActionsTest::new("root", components, Some(Moniker::root())).await;

        let res = test
            .create_dynamic_child_with_args(
                "coll",
                "dynamic_child",
                fcomponent::CreateChildArgs {
                    dynamic_offers: Some(vec![fdecl::Offer::Directory(fdecl::OfferDirectory {
                        source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
                        source_name: Some("foo".to_string()),
                        target_name: Some("foo".to_string()),
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
            )
            .await;
        assert_matches!(res, Err(fcomponent::Error::InvalidArguments));
    }

    #[fuchsia::test]
    async fn creating_dynamic_child_with_offer_target_set_fails() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .collection(
                    CollectionBuilder::new()
                        .name("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .build(),
        )];

        let test = ActionsTest::new("root", components, Some(Moniker::root())).await;

        let res = test
            .create_dynamic_child_with_args(
                "coll",
                "dynamic_child",
                fcomponent::CreateChildArgs {
                    dynamic_offers: Some(vec![fdecl::Offer::Directory(fdecl::OfferDirectory {
                        source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
                        source_name: Some("foo".to_string()),
                        target_name: Some("foo".to_string()),
                        target: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "dynamic_child".into(),
                            collection: Some("coll".parse().unwrap()),
                        })),
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
            )
            .await;
        assert_matches!(res, Err(fcomponent::Error::InvalidArguments));
    }

    async fn new_component() -> Arc<ComponentInstance> {
        ComponentInstance::new(
            Arc::new(Environment::empty()),
            InstancedMoniker::root(),
            "fuchsia-pkg://fuchsia.com/foo#at_root.cm".to_string(),
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstanceInterface::AboveRoot(Weak::new()),
            Arc::new(Hooks::new()),
            false,
        )
    }

    async fn new_resolved() -> InstanceState {
        let comp = new_component().await;
        let decl = ComponentDeclBuilder::new().build();
        let resolved_component = Component {
            resolved_url: "".to_string(),
            context_to_resolve_children: None,
            decl,
            package: None,
            config: None,
            abi_revision: None,
        };
        let ris = ResolvedInstanceState::new(
            &comp,
            resolved_component,
            ComponentAddress::from(&comp.component_url, &comp).await.unwrap(),
            Default::default(),
            ComponentInput::empty(),
        )
        .await
        .unwrap();
        InstanceState::Resolved(ris)
    }

    async fn new_unresolved() -> InstanceState {
        InstanceState::Unresolved(UnresolvedInstanceState::new(ComponentInput::empty()))
    }

    #[fuchsia::test]
    async fn instance_state_transitions_test() {
        // New --> Discovered.
        let mut is = InstanceState::New;
        is.set(new_unresolved().await);
        assert_matches!(is, InstanceState::Unresolved(_));

        // New --> Destroyed.
        let mut is = InstanceState::New;
        is.set(InstanceState::Destroyed);
        assert_matches!(is, InstanceState::Destroyed);

        // Discovered --> Resolved.
        let mut is = new_unresolved().await;
        is.set(new_resolved().await);
        assert_matches!(is, InstanceState::Resolved(_));

        // Discovered --> Destroyed.
        let mut is = new_unresolved().await;
        is.set(InstanceState::Destroyed);
        assert_matches!(is, InstanceState::Destroyed);

        // Resolved --> Discovered.
        let mut is = new_resolved().await;
        is.set(new_unresolved().await);
        assert_matches!(is, InstanceState::Unresolved(_));

        // Resolved --> Destroyed.
        let mut is = new_resolved().await;
        is.set(InstanceState::Destroyed);
        assert_matches!(is, InstanceState::Destroyed);
    }

    // Macro to make the panicking tests more readable.
    macro_rules! panic_test {
        (   [$(
                $test_name:ident( // Test case name.
                    $($args:expr),+$(,)? // Arguments for test case.
                )
            ),+$(,)?]
        ) => {
            $(paste::paste!{
                #[allow(non_snake_case)]
                #[fuchsia_async::run_until_stalled(test)]
                #[should_panic]
                async fn [< confirm_invalid_transition___ $test_name>]() {
                    confirm_invalid_transition($($args,)+).await;
                }
            })+
        }
    }

    async fn confirm_invalid_transition(cur: InstanceState, next: InstanceState) {
        let mut is = cur;
        is.set(next);
    }

    // Use the panic_test! macro to enumerate the invalid InstanceState transitions that are invalid
    // and should panic. As a result of the macro, the test names will be generated like
    // `confirm_invalid_transition___p2r`.
    panic_test!([
        // Destroyed !-> {Destroyed, Resolved, Discovered, New}..
        p2p(InstanceState::Destroyed, InstanceState::Destroyed),
        p2r(InstanceState::Destroyed, new_resolved().await),
        p2d(InstanceState::Destroyed, new_unresolved().await),
        p2n(InstanceState::Destroyed, InstanceState::New),
        // Resolved !-> {Resolved, New}.
        r2r(new_resolved().await, new_resolved().await),
        r2n(new_resolved().await, InstanceState::New),
        // Discovered !-> {Discovered, New}.
        d2d(new_unresolved().await, new_unresolved().await),
        d2n(new_unresolved().await, InstanceState::New),
        // New !-> {Resolved, New}.
        n2r(InstanceState::New, new_resolved().await),
        n2n(InstanceState::New, InstanceState::New),
    ]);

    #[fuchsia::test]
    async fn validate_and_convert_dynamic_offers() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .collection(
                    CollectionBuilder::new()
                        .name("col")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .build(),
        )];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();

        let _root = root
            .start_instance(&Moniker::root(), &StartReason::Root)
            .await
            .expect("failed to start root");
        test.runner.wait_for_urls(&["test:///root_resolved"]).await;

        let collection_decl = root
            .lock_resolved_state()
            .await
            .expect("failed to get resolved state")
            .resolved_component
            .decl
            .collections
            .iter()
            .find(|c| c.name.as_str() == "col")
            .expect("unable to find collection decl")
            .clone();

        let validate_and_convert = |offers: Vec<fdecl::Offer>| async {
            root.lock_resolved_state()
                .await
                .expect("failed to get resolved state")
                .validate_and_convert_dynamic_component(
                    Some(offers),
                    None,
                    &ChildDecl {
                        name: "foo".to_string(),
                        url: "http://foo".to_string(),
                        startup: fdecl::StartupMode::Lazy,
                        on_terminate: None,
                        environment: None,
                        config_overrides: None,
                    },
                    Some(&collection_decl),
                )
        };

        assert_eq!(
            validate_and_convert(vec![])
                .await
                .expect("failed to validate/convert dynamic offers")
                .0,
            vec![],
        );

        assert_eq!(
            validate_and_convert(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                source_name: Some("fuchsia.example.Echo".to_string()),
                target: None,
                target_name: Some("fuchsia.example.Echo".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                availability: Some(fdecl::Availability::Required),
                ..Default::default()
            })])
            .await
            .expect("failed to validate/convert dynamic offers")
            .0,
            vec![OfferDecl::Protocol(OfferProtocolDecl {
                source: OfferSource::Parent,
                source_name: "fuchsia.example.Echo".parse().unwrap(),
                source_dictionary: None,
                target: OfferTarget::Child(ChildRef {
                    name: "foo".into(),
                    collection: Some("col".parse().unwrap()),
                }),
                target_name: "fuchsia.example.Echo".parse().unwrap(),
                dependency_type: DependencyType::Strong,
                availability: Availability::Required,
            }),],
        );

        assert_eq!(
            validate_and_convert(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source: Some(fdecl::Ref::VoidType(fdecl::VoidRef {})),
                source_name: Some("fuchsia.example.Echo".to_string()),
                target: None,
                target_name: Some("fuchsia.example.Echo".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                availability: Some(fdecl::Availability::Optional),
                ..Default::default()
            })])
            .await
            .expect("failed to validate/convert dynamic offers")
            .0,
            vec![OfferDecl::Protocol(OfferProtocolDecl {
                source: OfferSource::Void,
                source_name: "fuchsia.example.Echo".parse().unwrap(),
                source_dictionary: None,
                target: OfferTarget::Child(ChildRef {
                    name: "foo".into(),
                    collection: Some("col".parse().unwrap()),
                }),
                target_name: "fuchsia.example.Echo".parse().unwrap(),
                dependency_type: DependencyType::Strong,
                availability: Availability::Optional,
            }),],
        );

        assert_matches!(
            validate_and_convert(vec![
                    fdecl::Offer::Protocol(fdecl::OfferProtocol {
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "doesnt-exist".to_string(),
                            collection: Some("col".parse().unwrap()),
                        })),
                        source_name: Some("fuchsia.example.Echo".to_string()),
                        source_dictionary: None,
                        target: None,
                        target_name: Some("fuchsia.example.Echo".to_string()),
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        availability: Some(fdecl::Availability::Optional),
                        ..Default::default()
                    })
                ])
                .await
                .expect_err("unexpected succeess in validate/convert dynamic offers"),
                AddChildError::DynamicOfferError { err }
            if err == DynamicOfferError::SourceNotFound {
                offer: OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Child(ChildRef {
                        name: "doesnt-exist".into(),
                        collection: Some("col".parse().unwrap()),
                    }),
                    source_name: "fuchsia.example.Echo".parse().unwrap(),
                    source_dictionary: None,
                    target: OfferTarget::Child(ChildRef {
                        name: "foo".into(),
                        collection: Some("col".parse().unwrap()),
                    }),
                    target_name: "fuchsia.example.Echo".parse().unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Optional,
                })
            }
        );
    }

    #[fuchsia::test]
    async fn validate_and_convert_dynamic_capabilities() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .collection(
                    CollectionBuilder::new()
                        .name("col")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .build(),
        )];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();

        let _root = root
            .start_instance(&Moniker::root(), &StartReason::Root)
            .await
            .expect("failed to start root");
        test.runner.wait_for_urls(&["test:///root_resolved"]).await;

        let validate_and_convert = |capabilities: Vec<fdecl::Capability>| async {
            root.lock_resolved_state()
                .await
                .expect("failed to get resolved state")
                .validate_and_convert_dynamic_component(
                    None,
                    Some(capabilities),
                    &ChildDecl {
                        name: "foo".to_string(),
                        url: "http://foo".to_string(),
                        startup: fdecl::StartupMode::Lazy,
                        on_terminate: None,
                        environment: None,
                        config_overrides: None,
                    },
                    None,
                )
        };

        assert_eq!(validate_and_convert(vec![]).await.unwrap().1, vec![],);

        assert_eq!(
            validate_and_convert(vec![fdecl::Capability::Config(fdecl::Configuration {
                name: Some("myConfig".to_string()),
                value: Some(fdecl::ConfigValue::Single(fdecl::ConfigSingleValue::Bool(true))),
                ..Default::default()
            })])
            .await
            .unwrap()
            .1,
            vec![cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                name: "myConfig".parse().unwrap(),
                value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Bool(true)),
            })],
        );

        assert_matches!(
            validate_and_convert(vec![
                fdecl::Capability::Config(fdecl::Configuration {
                name: Some("dupe".to_string()),
                value: Some(fdecl::ConfigValue::Single(fdecl::ConfigSingleValue::Bool(true))),
                ..Default::default()
            }),
            fdecl::Capability::Config(fdecl::Configuration {
                name: Some("dupe".to_string()),
                value: Some(fdecl::ConfigValue::Single(fdecl::ConfigSingleValue::Bool(true))),
                ..Default::default()
            }),
        ])
            .await.unwrap_err(),
            AddChildError::DynamicConfigError { err}
            if err ==
            cm_fidl_validator::error::ErrorList {

                                errs: vec![cm_fidl_validator::error::Error::duplicate_field(DeclType::Configuration, "name", "dupe")],
             }
        );
    }

    // Tests that logging in `with_logger_as_default` uses the LogSink routed to the component.
    #[fuchsia::test]
    async fn with_logger_as_default_uses_logsink() {
        const TEST_CHILD_NAME: &str = "child";

        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .protocol_default(flogger::LogSinkMarker::PROTOCOL_NAME)
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Self_,
                        source_name: flogger::LogSinkMarker::PROTOCOL_NAME.parse().unwrap(),
                        source_dictionary: None,
                        target_name: flogger::LogSinkMarker::PROTOCOL_NAME.parse().unwrap(),
                        target: OfferTarget::static_child(TEST_CHILD_NAME.to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .child_default(TEST_CHILD_NAME)
                    .build(),
            ),
            (
                TEST_CHILD_NAME,
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::protocol().name(flogger::LogSinkMarker::PROTOCOL_NAME))
                    .build(),
            ),
        ];
        let test_topology = ActionsTest::new(components[0].0, components, None).await;

        let (connect_tx, mut connect_rx) = mpsc::unbounded();
        let serve_logsink = move |mut stream: flogger::LogSinkRequestStream| {
            let connect_tx = connect_tx.clone();
            async move {
                while let Some(request) = stream.try_next().await.expect("failed to serve") {
                    match request {
                        flogger::LogSinkRequest::Connect { .. } => {
                            unimplemented!()
                        }
                        flogger::LogSinkRequest::ConnectStructured { .. } => {
                            connect_tx.unbounded_send(()).unwrap();
                        }
                        flogger::LogSinkRequest::WaitForInterestChange { .. } => {
                            // It's expected that the log publisher calls this, but it's not
                            // necessary to implement it.
                        }
                    }
                }
            }
        };

        // Serve LogSink from the root component.
        let mut root_out_dir = OutDir::new();
        root_out_dir.add_entry("/svc/fuchsia.logger.LogSink".parse().unwrap(), host(serve_logsink));
        test_topology.runner.add_host_fn("test:///root_resolved", root_out_dir.host_fn());

        let child = test_topology.look_up(vec![TEST_CHILD_NAME].try_into().unwrap()).await;

        // Start the child.
        ActionSet::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
        )
        .await
        .expect("failed to start child");

        assert!(child.is_started().await);

        // Log a message using the child's scoped logger.
        child.with_logger_as_default(|| info!("hello world")).await;

        // Wait for the logger to connect to LogSink.
        connect_rx.next().await.unwrap();
    }

    #[fuchsia::test]
    async fn find_resolved_test() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager().build())
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager().build())
                    .child(ChildBuilder::new().name("d").eager().build())
                    .build(),
            ),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();

        // Not resolved, so not found.
        assert_matches!(root.find_resolved(&vec!["a"].try_into().unwrap()).await, None);
        assert_matches!(root.find_resolved(&vec!["a", "b"].try_into().unwrap()).await, None);
        assert_matches!(root.find_resolved(&vec!["a", "b", "c"].try_into().unwrap()).await, None);
        assert_matches!(root.find_resolved(&vec!["a", "b", "d"].try_into().unwrap()).await, None);

        // Resolve each component.
        test.look_up(Moniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;

        // Now they can all be found.
        assert_matches!(root.find_resolved(&vec!["a"].try_into().unwrap()).await, Some(_));
        assert_eq!(
            root.find_resolved(&vec!["a"].try_into().unwrap()).await.unwrap().component_url,
            "test:///a",
        );
        assert_matches!(root.find_resolved(&vec!["a", "b"].try_into().unwrap()).await, Some(_));
        assert_matches!(
            root.find_resolved(&vec!["a", "b", "c"].try_into().unwrap()).await,
            Some(_)
        );
        assert_matches!(
            root.find_resolved(&vec!["a", "b", "d"].try_into().unwrap()).await,
            Some(_)
        );
        assert_matches!(
            root.find_resolved(&vec!["a", "b", "nonesuch"].try_into().unwrap()).await,
            None
        );

        // Unresolve, recursively.
        ActionSet::register(component_a.clone(), UnresolveAction::new())
            .await
            .expect("unresolve failed");

        // Unresolved recursively, so children in Discovered state.
        assert!(is_discovered(&component_a).await);
        assert!(is_discovered(&component_b).await);
        assert!(is_discovered(&component_c).await);
        assert!(is_discovered(&component_d).await);

        assert_matches!(root.find_resolved(&vec!["a"].try_into().unwrap()).await, None);
        assert_matches!(root.find_resolved(&vec!["a", "b"].try_into().unwrap()).await, None);
        assert_matches!(root.find_resolved(&vec!["a", "b", "c"].try_into().unwrap()).await, None);
        assert_matches!(root.find_resolved(&vec!["a", "b", "d"].try_into().unwrap()).await, None);
    }

    /// While the provider component is stopping, opening its outgoing directory should not block.
    /// This is important to not cause deadlocks if we are draining the provider component's
    /// namespace.
    #[fuchsia::test(allow_stalls = false)]
    async fn open_outgoing_while_component_is_stopping() {
        // Use mock time in this test.
        let initial = fasync::Time::from_nanos(0);
        TestExecutor::advance_to(initial).await;

        let components = vec![("root", ComponentDeclBuilder::new().build())];
        let test_topology = ActionsTest::new(components[0].0, components, None).await;

        let root_out_dir = OutDir::new();
        test_topology.runner.add_host_fn("test:///root_resolved", root_out_dir.host_fn());

        // Configure the component runner to take 3 seconds to stop the component.
        let response_delay = zx::Duration::from_seconds(3);
        test_topology.runner.add_controller_response(
            "test:///root_resolved",
            Box::new(move || ControllerActionResponse {
                close_channel: true,
                delay: Some(response_delay),
            }),
        );

        let root = test_topology.look_up(Moniker::default()).await;
        assert!(!root.is_started().await);

        // Start the component.
        let root = root
            .start_instance(&Moniker::root(), &StartReason::Root)
            .await
            .expect("failed to start root");
        test_topology.runner.wait_for_urls(&["test:///root_resolved"]).await;

        // Start to stop the component. This will stall because the framework will be
        // waiting the controller to respond.
        let stop_fut = ActionSet::register(root.clone(), StopAction::new(false));
        futures::pin_mut!(stop_fut);
        assert_matches!(TestExecutor::poll_until_stalled(&mut stop_fut).await, Poll::Pending);

        // Open the outgoing directory. This should not block.
        let (_, mut server_end) = zx::Channel::create();
        let open_fut = root.open_outgoing(fio::OpenFlags::empty(), ".", &mut server_end);
        futures::pin_mut!(open_fut);
        // Note: if the behavior of `open_outgoing` changes to start the component, we can simply
        // update the `Err` here accordingly.
        assert_matches!(
            TestExecutor::poll_until_stalled(open_fut).await,
            Poll::Ready(Err(OpenOutgoingDirError::InstanceNotRunning))
        );

        // Let the timer advance. The component should be stopped now.
        TestExecutor::advance_to(initial + response_delay).await;
        assert_matches!(stop_fut.await, Ok(()));

        // Open the outgoing directory. This should still not block.
        let (_, mut server_end) = zx::Channel::create();
        let _ = root.open_outgoing(fio::OpenFlags::empty(), ".", &mut server_end).await;
    }
}
