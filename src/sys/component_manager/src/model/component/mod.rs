// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod instance;
pub mod manager;

use {
    crate::{
        bedrock::program::StopRequestSuccess,
        framework::controller,
        model::{
            actions::{
                start, ActionSet, DestroyAction, ResolveAction, ShutdownAction, ShutdownType,
                StartAction, UnresolveAction,
            },
            context::ModelContext,
            environment::Environment,
            error::{
                ActionError, AddDynamicChildError, DestroyActionError, ModelError,
                OpenExposedDirError, OpenOutgoingDirError, ResolveActionError, StartActionError,
                StopActionError, StructuredConfigError,
            },
            hooks::{Event, EventPayload, Hooks},
            routing::{
                self,
                router::{Request, Routable, Router},
                RoutingError,
            },
            start::Start,
        },
    },
    ::namespace::Entry as NamespaceEntry,
    ::routing::{
        component_instance::{
            ComponentInstanceInterface, ExtendedInstanceInterface, ResolvedInstanceInterface,
            WeakComponentInstanceInterface, WeakExtendedInstanceInterface,
        },
        error::ComponentInstanceError,
        policy::GlobalPolicyChecker,
        resolving::{ComponentResolutionContext, ResolvedComponent, ResolvedPackage},
    },
    async_trait::async_trait,
    bedrock_error::{BedrockError, Explain},
    cm_moniker::{IncarnationId, InstancedMoniker},
    cm_rust::{ChildDecl, CollectionDecl, ComponentDecl, UseDecl, UseStorageDecl},
    cm_types::Name,
    cm_util::TaskGroup,
    component_id_index::InstanceId,
    config_encoder::ConfigFields,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio,
    fidl_fuchsia_process as fprocess, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::{
        future::{join_all, BoxFuture},
        lock::{MappedMutexGuard, Mutex, MutexGuard},
    },
    instance::{
        InstanceState, ResolvedInstanceState, ShutdownInstanceState, StartedInstanceState,
        StopOutcomeWithEscrow,
    },
    manager::ComponentManagerInstance,
    moniker::{ChildName, ChildNameBase, Moniker, MonikerBase},
    sandbox::{Capability, Dict, DictEntries, Open},
    std::{
        clone::Clone,
        collections::{HashMap, HashSet},
        fmt,
        ops::DerefMut,
        sync::{Arc, Weak},
        time::Duration,
    },
    tracing::{debug, error, warn},
    version_history::AbiRevision,
    vfs::{
        directory::entry::{DirectoryEntry, DirectoryEntryAsync, EntryInfo, OpenRequest},
        execution_scope::ExecutionScope,
    },
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
    /// Indicates that the component is starting because of a request to its outgoing
    /// directory.
    OutgoingDirectory,
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
                StartReason::OutgoingDirectory => {
                    "Instance started due to a request to its outgoing directory".to_string()
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
    pub instanced_moniker: InstancedMoniker,
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
    /// Actions on the instance that must eventually be completed.
    actions: Mutex<ActionSet>,
    /// Tasks owned by this component instance that will be cancelled if the component is
    /// destroyed.
    nonblocking_task_group: TaskGroup,
    /// Tasks owned by this component instance that will block destruction if the component is
    /// destroyed.
    blocking_task_group: TaskGroup,
    /// The ExecutionScope for this component. Pseudo directories should be hosted with this scope
    /// to tie their life-time to that of the component. Tasks can block component destruction by
    /// using `active_guard()` (as an alternative to `blocking_task_group`).
    pub execution_scope: ExecutionScope,
}

impl ComponentInstance {
    /// Instantiates a new root component instance.
    pub async fn new_root(
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
        .await
    }

    /// Instantiates a new component instance with the given contents.
    // TODO(https://fxbug.dev/42077692) convert this to a builder API
    pub async fn new(
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
            actions: Mutex::new(ActionSet::new()),
            hooks,
            nonblocking_task_group: TaskGroup::new(),
            blocking_task_group: TaskGroup::new(),
            persistent_storage,
            execution_scope: ExecutionScope::new(),
        })
    }

    /// Locks and returns the instance's mutable state.
    // TODO(b/309656051): Remove this method from ComponentInstance's public API
    pub async fn lock_state(&self) -> MutexGuard<'_, InstanceState> {
        self.state.lock().await
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
        self.lock_state().await.is_started()
    }

    /// Locks and returns a lazily resolved and populated `ResolvedInstanceState`. Does not
    /// register a `Resolve` action unless the resolved state is not already populated, so this
    /// function can be called re-entrantly from a Resolved hook. Returns an `InstanceNotFound`
    /// error if the instance is destroyed.
    // TODO(b/309656051): Remove this method from ComponentInstance's public API
    pub async fn lock_resolved_state<'a>(
        self: &'a Arc<Self>,
    ) -> Result<MappedMutexGuard<'a, InstanceState, ResolvedInstanceState>, ActionError> {
        loop {
            /// Returns Ok(Some(_)) when the component is in a resolved state, Ok(None) when the
            /// component is in a state from which it can be resolved, and Err(_) when the
            /// component is in a state from which it cannot be resolved.
            async fn get_mapped_mutex_or_error<'a>(
                self_: &'a Arc<ComponentInstance>,
            ) -> Result<
                Option<MappedMutexGuard<'a, InstanceState, ResolvedInstanceState>>,
                ActionError,
            > {
                let state = self_.state.lock().await;
                if state.get_resolved_state().is_some() {
                    return Ok(Some(MutexGuard::map(state, |s| {
                        s.get_resolved_state_mut().expect("not resolved")
                    })));
                }
                if let InstanceState::Destroyed = *state {
                    return Err(ResolveActionError::InstanceDestroyed {
                        moniker: self_.moniker.clone(),
                    }
                    .into());
                }
                if state.is_shut_down() {
                    return Err(ResolveActionError::InstanceShutDown {
                        moniker: self_.moniker.clone(),
                    }
                    .into());
                }
                Ok(None)
            }

            if let Some(mapped_guard) = get_mapped_mutex_or_error(&self).await? {
                return Ok(mapped_guard);
            }
            self.resolve().await?;
            if let Some(mapped_guard) = get_mapped_mutex_or_error(&self).await? {
                return Ok(mapped_guard);
            }
            // If we've reached here, then the component must have been unresolved in-between our
            // calls to resolved and get_mapped_mutex_or_error. Our mission here remains to resolve
            // the component if necessary and then return the resolved state, so let's loop and try
            // to resolve it again.
        }
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
                std::mem::replace(&mut *entries, DictEntries::new())
            };
            let capabilities = child_input.capabilities();
            let mut child_dict_entries = capabilities.lock_entries();
            for (key, value) in dict_entries.into_iter() {
                // The child/collection Dict normally contains Routers created by component manager.
                // ChildArgs.dict may contain capabilities created by an external client.
                //
                // Currently there is no way to create a Rotuer externally, so assume these
                // are Sender capabilities and convert them to Router here.
                //
                // TODO(https://fxbug.dev/319542502): Consider using the external Router type, once
                // it exists
                let router = match value {
                    Capability::Sender(s) => Router::new_ok(s),
                    _ => return Err(AddDynamicChildError::InvalidDictionary),
                };

                if let Err(_) =
                    child_dict_entries.insert(key.clone(), Capability::Router(Box::new(router)))
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
            let resolved_state = state
                .get_resolved_state()
                .ok_or(DestroyActionError::InstanceNotResolved { moniker: self.moniker.clone() })?;
            if let Some(c) = resolved_state.get_child(&child_moniker) {
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
        ActionSet::register(self.clone(), crate::model::actions::StopAction::new(false)).await
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
    /// instance is to be put into `InstanceState::Resolved` or `InstanceState::Shutdown`.
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
        // If the component is started, we first move it back to the resolved state. We will move
        // it to the shutdown state after the stopping is complete.
        let mut runtime = None;
        self.lock_state().await.replace(|instance_state| match instance_state {
            InstanceState::Started(resolved_state, started_state) => {
                runtime = Some(started_state);
                InstanceState::Resolved(resolved_state)
            }
            other_state => other_state,
        });

        let stop_result = {
            if let Some(runtime) = &mut runtime {
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
                if ret.outcome.request == StopRequestSuccess::KilledAfterTimeout
                    || ret.outcome.request == StopRequestSuccess::Killed
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
                    execution_controller_task.set_stop_status(ret.outcome.component_exit_status);
                }
                Some(ret)
            } else {
                None
            }
        };

        // TODO(b/322564390): Move program_input_dict_additions into `StartedInstanceState` to avoid locking InstanceState.
        {
            let mut state = self.lock_state().await;
            if let Some(resolved_state) = state.get_resolved_state_mut() {
                resolved_state.program_input_dict_additions = None;
            };
        }

        // When the component is stopped, any child instances in collections must be destroyed.
        self.destroy_dynamic_children()
            .await
            .map_err(|err| StopActionError::DestroyDynamicChildrenFailed { err: Box::new(err) })?;

        if let Some(StopOutcomeWithEscrow { outcome, escrow_request }) = stop_result {
            // Store any escrowed state.
            {
                let mut state = self.lock_state().await;
                if let InstanceState::Resolved(resolved_state) = &mut *state {
                    if let Some(program_escrow) = resolved_state.program_escrow() {
                        program_escrow.did_stop(escrow_request);
                    }
                };
            }

            let event =
                Event::new(self, EventPayload::Stopped { status: outcome.component_exit_status });
            self.hooks.dispatch(&event).await;
        }

        if shut_down {
            self.move_state_to_shutdown().await?;
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

    /// Moves the state of `self` to `InstanceState::Shutdown`, or panics. If the component was in
    /// the `Started` state, the `StartedInstanceState` is returned.
    async fn move_state_to_shutdown(self: &Arc<Self>) -> Result<(), StopActionError> {
        loop {
            fn get_storage_uses(resolved_state: &ResolvedInstanceState) -> Vec<UseStorageDecl> {
                resolved_state
                    .resolved_component
                    .decl
                    .uses
                    .iter()
                    .filter_map(|use_| match use_ {
                        UseDecl::Storage(ref storage_use) => Some(storage_use.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            }

            // If the component is in a resolved state, then we have to route its storage
            // capabilities. We shouldn't do this while holding the state lock, so let's do this in
            // advance before grabbing the state lock below.
            let mut routed_storage = vec![];
            let storage_uses = {
                let state = self.lock_state().await;
                match &*state {
                    InstanceState::Resolved(resolved_state) => get_storage_uses(&resolved_state),
                    _ => vec![],
                }
            };
            for storage_use in &storage_uses {
                if let Ok(info) = routing::route_storage(storage_use.clone(), &self).await {
                    routed_storage.push(info);
                }
            }

            // Now that any necessary routing operations are out of the way, grab the state lock
            // and let's calculate our new state.
            let mut state = self.lock_state().await;
            let new_state = match state.deref_mut() {
                InstanceState::New => {
                    panic!("component should be discovered before shutting down");
                }
                InstanceState::Unresolved(unresolved_state) => Some(InstanceState::Shutdown(
                    ShutdownInstanceState { children: HashMap::new(), routed_storage: vec![] },
                    unresolved_state.take(),
                )),
                InstanceState::Resolved(resolved_state) => {
                    let children = resolved_state.children.clone();
                    if storage_uses != get_storage_uses(&resolved_state) {
                        continue;
                    }
                    Some(InstanceState::Shutdown(
                        ShutdownInstanceState { children, routed_storage },
                        resolved_state.to_unresolved(),
                    ))
                }
                InstanceState::Started(_, _) => {
                    error!("component {} was started while it was stopping or shutting down, this should be impossible", &self.moniker);
                    return Err(StopActionError::ComponentStartedDuringShutdown);
                }
                InstanceState::Shutdown(_, _) | InstanceState::Destroyed => None,
            };
            if let Some(new_state) = new_state {
                state.set(new_state);
            }
            return Ok(());
        }
    }

    async fn destroy_child_if_single_run(
        self: &Arc<Self>,
        child_moniker: &ChildName,
        incarnation: IncarnationId,
    ) {
        let single_run_colls = {
            let state = self.lock_state().await;
            if state.get_resolved_state().is_none() {
                // Component instance was not resolved, so no dynamic children.
                return;
            }
            let resolved_state = state.get_resolved_state().unwrap();
            resolved_state
                .decl()
                .collections
                .iter()
                .filter_map(|c| match c.durability {
                    fdecl::Durability::SingleRun => Some(c.name.clone()),
                    fdecl::Durability::Transient => None,
                })
                .collect::<HashSet<_>>()
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
        let routed_storage = {
            let mut state = self.lock_state().await;
            match *state {
                InstanceState::Shutdown(ref mut s, _) => s.routed_storage.drain(..).collect::<Vec<_>>(),
                _ => panic!("cannot destroy component instance {} because it is not shutdown, it is in state {:?}", self.moniker, *state),
            }
        };
        for storage in routed_storage {
            match routing::delete_storage(storage).await {
                Ok(()) => (),
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
        Ok(())
    }

    /// Registers actions to destroy all dynamic children of collections belonging to this instance.
    async fn destroy_dynamic_children(self: &Arc<Self>) -> Result<(), ActionError> {
        let moniker_incarnations: Vec<_> = {
            match *self.lock_state().await {
                InstanceState::Resolved(ref state) | InstanceState::Started(ref state, _) => {
                    state.children().map(|(k, c)| (k.clone(), c.incarnation_id())).collect()
                }
                InstanceState::Shutdown(ref state, _) => {
                    state.children.iter().map(|(k, c)| (k.clone(), c.incarnation_id())).collect()
                }
                _ => {
                    // Component instance was not resolved, so no dynamic children.
                    return Ok(());
                }
            }
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
                InstanceState::Resolved(ref s) | InstanceState::Started(ref s, _) => {
                    let child = s.get_child(&moniker).map(|r| r.clone());
                    child
                }
                InstanceState::Shutdown(ref state, _) => {
                    state.children.get(&moniker).map(|r| r.clone())
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

    /// Opens an object referenced by `path` from the outgoing directory of the component.  The
    /// component must have a program, or this method will fail.  Starts the component if necessary.
    ///
    /// TODO(https://fxbug.dev/332329856): If the component is to be started as a result of the open
    /// call, and the starting failed, that error is not returned here. If you would like to observe
    /// start errors, call `ensure_started` before this function.
    pub async fn open_outgoing(
        &self,
        open_request: OpenRequest<'_>,
    ) -> Result<(), OpenOutgoingDirError> {
        match *self.lock_state().await {
            InstanceState::Resolved(ref mut resolved)
            | InstanceState::Started(ref mut resolved, _) => {
                let program_escrow =
                    resolved.program_escrow().ok_or(OpenOutgoingDirError::InstanceNonExecutable)?;
                program_escrow.open_outgoing(open_request)?;
                Ok(())
            }
            _ => Err(OpenOutgoingDirError::InstanceNotResolved),
        }
    }

    /// Returns an [`Open`] representation of the outgoing directory of the component. It performs
    /// the same checks as `open_outgoing`, but errors are surfaced at the server endpoint.
    pub fn get_outgoing(self: &Arc<Self>) -> Open {
        struct GetOutgoing {
            component: WeakComponentInstance,
        }

        impl DirectoryEntry for GetOutgoing {
            fn entry_info(&self) -> EntryInfo {
                EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
            }

            fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
                let component = self.component.upgrade().map_err(|e| e.as_zx_status())?;
                request.spawn(component);
                Ok(())
            }
        }

        Open::new(Arc::new(GetOutgoing { component: WeakComponentInstance::from(self) }))
    }

    /// Obtains the program output dict.
    pub async fn get_program_output_dict(self: &Arc<Self>) -> Result<Dict, BedrockError> {
        Ok(self.lock_resolved_state().await?.program_output_dict.clone())
    }

    /// Returns a router that delegates to the program output dict.
    ///
    /// This will be helpful in breaking up reference cycles. For example, you can insert
    /// an item into the program output dict that references another item in the same dict,
    /// by indirecting through this router.
    pub fn program_output(self: &Arc<Self>) -> Router {
        #[derive(Debug)]
        struct ProgramOutput {
            component: WeakComponentInstance,
        }

        #[async_trait]
        impl Routable for ProgramOutput {
            async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
                let component = self.component.upgrade().map_err(RoutingError::from)?;
                component.get_program_output_dict().await?.route(request).await
            }
        }

        Router::new(ProgramOutput { component: self.as_weak() })
    }

    /// Obtains the component output dict.
    pub async fn get_component_output_dict(self: &Arc<Self>) -> Result<Dict, BedrockError> {
        Ok(self.lock_resolved_state().await?.component_output_dict.clone())
    }

    /// Returns a router that delegates to the component output dict.
    pub fn component_output(self: &Arc<Self>) -> Router {
        #[derive(Debug)]
        struct ComponentOutput {
            component: WeakComponentInstance,
        }

        #[async_trait]
        impl Routable for ComponentOutput {
            async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
                let component = self.component.upgrade().map_err(RoutingError::from)?;
                component.get_component_output_dict().await?.route(request).await
            }
        }

        Router::new(ComponentOutput { component: self.as_weak() })
    }

    /// Opens this instance's exposed directory if it has been resolved.
    pub async fn open_exposed(
        &self,
        open_request: OpenRequest<'_>,
    ) -> Result<(), OpenExposedDirError> {
        let state = self.lock_state().await;
        match &*state {
            InstanceState::New | InstanceState::Unresolved(_) | InstanceState::Shutdown(_, _) => {
                Err(OpenExposedDirError::InstanceNotResolved)
            }
            InstanceState::Resolved(resolved_instance_state)
            | InstanceState::Started(resolved_instance_state, _) => {
                resolved_instance_state.get_exposed_dir().await.open_entry(open_request)?;
                Ok(())
            }
            InstanceState::Destroyed => Err(OpenExposedDirError::InstanceDestroyed),
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
            if let Some(res) = start::should_return_early(&state, &self.moniker) {
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
                InstanceState::Resolved(ref s) | InstanceState::Started(ref s, _) => s
                    .children()
                    .filter_map(|(_, r)| match r.startup {
                        fdecl::StartupMode::Eager => Some(r.clone()),
                        fdecl::StartupMode::Lazy => None,
                    })
                    .collect(),
                InstanceState::Shutdown(_, _) => {
                    return Err(StartActionError::InstanceShutDown {
                        moniker: self.moniker.clone(),
                    }
                    .into());
                }
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
        let state = self.lock_state().await;
        match state.get_started_state() {
            Some(StartedInstanceState { logger: Some(ref logger), .. }) => {
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
        let mut state = self.lock_state().await;
        state.scope_server_end(server_end);
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
    pub fn persistent_storage_for_child(&self, collection: Option<&CollectionDecl>) -> bool {
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
            let next = cur
                .lock_state()
                .await
                .get_resolved_state()
                .and_then(|r| r.get_child(moniker))
                .cloned()?;
            cur = next
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
            let next = cur
                .lock_state()
                .await
                .get_resolved_state()
                .and_then(|r| r.get_child(moniker))
                .cloned()?;
            cur = next
        }
        // Found the moniker, the last child in the chain of resolved parents. Is it resolved?
        if cur.lock_state().await.get_resolved_state().is_some() {
            Some(cur.clone())
        } else {
            None
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

impl DirectoryEntry for ComponentInstance {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }

    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.spawn(self);
        Ok(())
    }
}

impl DirectoryEntryAsync for ComponentInstance {
    async fn open_entry_async(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        self.open_outgoing(request).await.map_err(|e| e.as_zx_status())
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

    fn environment(&self) -> &::routing::environment::Environment<Self> {
        self.environment.environment()
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

#[cfg(test)]
pub mod tests {
    use {
        super::*,
        crate::model::{
            actions::shutdown,
            actions::test_utils::is_discovered,
            actions::StopAction,
            error::{AddChildError, DynamicOfferError},
            events::{registry::EventSubscription, stream::EventStream},
            hooks::EventType,
            structured_dict::ComponentInput,
            testing::{
                mocks::ControllerActionResponse,
                out_dir::OutDir,
                routing_test_helpers::{RoutingTest, RoutingTestBuilder},
                test_helpers::{component_decl_with_test_runner, ActionsTest, ComponentInfo},
            },
        },
        ::routing::resolving::ComponentAddress,
        assert_matches::assert_matches,
        cm_fidl_validator::error::DeclType,
        cm_rust::{
            Availability, ChildRef, DependencyType, ExposeSource, OfferDecl, OfferProtocolDecl,
            OfferSource, OfferTarget, UseEventStreamDecl, UseSource,
        },
        cm_rust_testing::*,
        fasync::TestExecutor,
        fidl::endpoints::DiscoverableProtocolMarker,
        fidl_fuchsia_logger as flogger, fuchsia_async as fasync, fuchsia_zircon as zx,
        fuchsia_zircon::AsHandleRef,
        futures::{channel::mpsc, FutureExt, StreamExt, TryStreamExt},
        instance::UnresolvedInstanceState,
        routing_test_helpers::component_id_index::make_index_file,
        std::{panic, task::Poll},
        tracing::info,
        vfs::{path::Path as VfsPath, service::host, ToObjectRequest},
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
        let resolved_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Resolved).await;
        let started_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Started).await;
        let debug_started_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::DebugStarted).await;

        assert!(resolved_timestamp < started_timestamp);
        assert!(started_timestamp == debug_started_timestamp);

        let component = bind_handle.await;
        let component_timestamp =
            component.lock_state().await.get_started_state().unwrap().timestamp;
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
        let _component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let _component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let _component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;

        // Just unresolve component a and children
        assert_matches!(component_a.unresolve().await, Ok(()));
        // component a is now resolved
        assert!(is_discovered(&component_a).await);
        // Component a no longer has children, due to not being resolved
        assert_matches!(component_a.find(&vec!["b"].try_into().unwrap()).await, None);
        assert_matches!(component_a.find(&vec!["b", "c"].try_into().unwrap()).await, None);
        assert_matches!(component_a.find(&vec!["b", "d"].try_into().unwrap()).await, None);

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
        let example_offer = OfferBuilder::directory()
            .name("foo")
            .source_static_child("a")
            .target_static_child("b")
            .build();
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
        let example_offer = OfferBuilder::directory()
            .name("foo")
            .source_static_child("a")
            .target_static_child("b")
            .build();

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
            source_dictionary: Default::default(),
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
            source_dictionary: Default::default(),
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
        let example_offer = OfferBuilder::service()
            .name("foo")
            .source(OfferSource::Collection("coll".parse().unwrap()))
            .target_static_child("static_child")
            .build();

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
        let static_collection_offer = OfferBuilder::service()
            .name("foo")
            .source(OfferSource::Collection("coll1".parse().unwrap()))
            .target(OfferTarget::Collection("coll2".parse().unwrap()))
            .build();

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
        .await
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
            Default::default(),
        )
        .await
        .unwrap();
        InstanceState::Resolved(ris)
    }

    async fn new_unresolved() -> InstanceState {
        InstanceState::Unresolved(UnresolvedInstanceState::new(ComponentInput::default()))
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
                source_dictionary: Default::default(),
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
                source_dictionary: Default::default(),
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
                        source_dictionary: Default::default(),
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
                    source_dictionary: Default::default(),
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
                    .offer(
                        OfferBuilder::protocol()
                            .name(flogger::LogSinkMarker::PROTOCOL_NAME)
                            .source(OfferSource::Self_)
                            .target_static_child(TEST_CHILD_NAME),
                    )
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
        let _component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let _component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let _component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;

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

        // Unresolve component a, this causes it to stop having children and drop component b after
        // shutting it down.
        ActionSet::register(component_a.clone(), UnresolveAction::new())
            .await
            .expect("unresolve failed");

        // Because component a is not resolved, it does not have children
        assert_matches!(component_a.find(&vec!["b"].try_into().unwrap()).await, None);
        assert_matches!(component_a.find(&vec!["b", "c"].try_into().unwrap()).await, None);
        assert_matches!(component_a.find(&vec!["b", "d"].try_into().unwrap()).await, None);
    }

    /// If a component is not started, a call to `open_outgoing` should start the component
    /// and deliver the open request there.
    #[fuchsia::test]
    async fn open_outgoing_starts_component() {
        let components = vec![("root", ComponentDeclBuilder::new().build())];
        let test_topology = ActionsTest::new(components[0].0, components, None).await;
        let (open_request_tx, mut open_request_rx) = mpsc::unbounded();

        let mut root_out_dir = OutDir::new();
        root_out_dir.add_entry(
            "/svc/foo".parse().unwrap(),
            vfs::service::endpoint(move |_scope, channel| {
                open_request_tx.unbounded_send(channel).unwrap();
            }),
        );
        test_topology.runner.add_host_fn("test:///root_resolved", root_out_dir.host_fn());

        let root = test_topology.look_up(Moniker::default()).await;
        assert!(!root.is_started().await);

        let (client_end, server_end) = zx::Channel::create();
        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        root.open_outgoing(OpenRequest::new(
            execution_scope.clone(),
            fio::OpenFlags::empty(),
            "svc/foo".try_into().unwrap(),
            &mut object_request,
        ))
        .await
        .unwrap();
        let server_end = open_request_rx.next().await.unwrap();
        assert!(root.is_started().await);
        assert_eq!(
            client_end.basic_info().unwrap().related_koid,
            server_end.basic_info().unwrap().koid
        );
    }

    /// If a component is not started and is configured incorrectly to not be able to start,
    /// `open_outgoing` should succeed but the channel is closed.
    #[fuchsia::test]
    async fn open_outgoing_failed_to_start_component() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new_empty_component().add_program("invalid").build(),
        )];
        let test_topology = ActionsTest::new(components[0].0, components, None).await;

        let mut root_out_dir = OutDir::new();
        root_out_dir.add_entry(
            "/svc/foo".parse().unwrap(),
            vfs::service::endpoint(move |_scope, _channel| {
                unreachable!();
            }),
        );
        test_topology.runner.add_host_fn("test:///root_resolved", root_out_dir.host_fn());

        let root = test_topology.look_up(Moniker::default()).await;
        assert!(!root.is_started().await);

        let (client_end, server_end) = zx::Channel::create();

        let execution_scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_matches!(
            root.open_outgoing(OpenRequest::new(
                execution_scope.clone(),
                fio::OpenFlags::empty(),
                "svc/foo".try_into().unwrap(),
                &mut object_request,
            ))
            .await,
            Ok(())
        );
        assert!(!root.is_started().await);

        fasync::OnSignals::new(&client_end, zx::Signals::CHANNEL_PEER_CLOSED).await.unwrap();
        assert!(!root.is_started().await);
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
        let (_, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_matches!(
            root.open_outgoing(OpenRequest::new(
                scope.clone(),
                fio::OpenFlags::empty(),
                VfsPath::dot(),
                &mut object_request
            ))
            .await,
            Ok(())
        );

        // Let the timer advance. The component should be stopped now.
        TestExecutor::advance_to(initial + response_delay).await;
        assert_matches!(stop_fut.await, Ok(()));

        // Open the outgoing directory. This should still not block.
        let (_, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        assert_matches!(
            root.open_outgoing(OpenRequest::new(
                scope.clone(),
                fio::OpenFlags::empty(),
                VfsPath::dot(),
                &mut object_request
            ))
            .await,
            Ok(())
        );
    }
}
