// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_arch = "aarch64")]
use builtins::smc_resource::SmcResource;

#[cfg(target_arch = "x86_64")]
use builtins::ioport_resource::IoportResource;
use fidl_fuchsia_boot::UserbootRequest;

use crate::bootfs::BootfsSvc;
use crate::builtin::builtin_resolver::{BuiltinResolver, SCHEME as BUILTIN_SCHEME};
use crate::builtin::builtin_runner::BuiltinProgramGen;
use crate::builtin::crash_introspect::CrashIntrospectSvc;
use crate::builtin::fuchsia_boot_resolver::{
    FuchsiaBootPackageResolver, FuchsiaBootResolver, SCHEME as BOOT_SCHEME,
};
use crate::builtin::log::{ReadOnlyLog, WriteOnlyLog};
use crate::builtin::realm_builder::{
    RealmBuilderResolver, RealmBuilderRunnerFactory, RUNNER_NAME as REALM_BUILDER_RUNNER_NAME,
    SCHEME as REALM_BUILDER_SCHEME,
};
use crate::builtin::runner::{BuiltinRunner, BuiltinRunnerFactory};
use crate::builtin::svc_stash_provider::SvcStashCapability;
use crate::builtin::system_controller::SystemController;
use crate::builtin::time::{create_utc_clock, UtcInstantMaintainer};
use crate::capability::{self, BuiltinCapability, FrameworkCapability};
use crate::framework::binder::BinderFrameworkCapability;
use crate::framework::capability_store::CapabilityStore;
use crate::framework::config_override::ConfigOverride;
use crate::framework::introspector::IntrospectorFrameworkCapability;
use crate::framework::lifecycle_controller::LifecycleController;
use crate::framework::namespace::Namespace;
use crate::framework::pkg_dir::PkgDirectoryFrameworkCapability;
use crate::framework::realm::Realm;
use crate::framework::realm_query::RealmQuery;
use crate::framework::route_validator::RouteValidatorFrameworkCapability;
use crate::inspect_sink_provider::InspectSinkProvider;
use crate::model::component::manager::ComponentManagerInstance;
use crate::model::component::WeakComponentInstance;
use crate::model::environment::Environment;
use crate::model::event_logger::EventLogger;
use crate::model::events::registry::{EventRegistry, EventSubscription};
use crate::model::events::serve::serve_event_stream;
use crate::model::events::source_factory::{EventSourceFactory, EventSourceFactoryCapability};
use crate::model::events::stream_provider::EventStreamProvider;
use crate::model::model::{Model, ModelParams};
use crate::model::resolver::{Resolver, ResolverRegistry};
use crate::model::token::InstanceRegistry;
use crate::root_stop_notifier::RootStopNotifier;
use crate::sandbox_util::{take_handle_as_stream, LaunchTaskOnReceive};
use ::diagnostics::lifecycle::ComponentLifecycleTimeStats;
use ::diagnostics::task_metrics::ComponentTreeStats;
use ::routing::bedrock::dict_ext::DictExt;
use ::routing::bedrock::structured_dict::ComponentInput;
use ::routing::bedrock::with_porcelain_type::WithPorcelainType;
use ::routing::capability_source::{
    BuiltinSource, CapabilitySource, ComponentCapability, InternalCapability, NamespaceSource,
};
use ::routing::component_instance::{ComponentInstanceInterface, TopInstanceInterface};
use ::routing::environment::{DebugRegistry, RunnerRegistry};
use ::routing::policy::{GlobalPolicyChecker, ScopedPolicyChecker};
use anyhow::{format_err, Context as _, Error};
use builtins::arguments::Arguments as BootArguments;
use builtins::cpu_resource::CpuResource;
use builtins::debug_resource::DebugResource;
use builtins::debuglog_resource::DebuglogResource;
use builtins::energy_info_resource::EnergyInfoResource;
use builtins::factory_items::FactoryItems;
use builtins::hypervisor_resource::HypervisorResource;
use builtins::info_resource::InfoResource;
use builtins::iommu_resource::IommuResource;
use builtins::irq_resource::IrqResource;
use builtins::items::Items;
use builtins::kernel_stats::KernelStats;
use builtins::mexec_resource::MexecResource;
use builtins::mmio_resource::MmioResource;
use builtins::msi_resource::MsiResource;
use builtins::power_resource::PowerResource;
use builtins::profile_resource::ProfileResource;
use builtins::root_job::RootJob;
use builtins::stall_resource::StallResource;
use builtins::vmex_resource::VmexResource;
use cm_config::{RuntimeConfig, SecurityPolicy, VmexSource};
use cm_rust::{
    Availability, CapabilityTypeName, RunnerRegistration, UseEventStreamDecl, UseSource,
};
use cm_types::{Name, Url};
use cm_util::WeakTaskGroup;
use elf_runner::crash_info::CrashRecords;
use elf_runner::process_launcher::ProcessLauncher;
use elf_runner::vdso_vmo::{get_next_vdso_vmo, get_stable_vdso_vmo, get_vdso_vmo};
use fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker, RequestStream, ServerEnd};
use fidl_fuchsia_component_internal::BuiltinBootResolver;
use fidl_fuchsia_diagnostics_types::Task as DiagnosticsTask;
use fuchsia_component::server::*;
use fuchsia_inspect::health::Reporter;
use fuchsia_inspect::stats::InspectorExt;
use fuchsia_inspect::{component, Inspector};
use fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType, UtcClock};
use fuchsia_zbi::{ZbiParser, ZbiType};
use futures::future::{self, BoxFuture};
use futures::{FutureExt, StreamExt, TryStreamExt};
use hooks::EventType;
use log::{error, info, warn};
use moniker::ExtendedMoniker;
use routing::resolving::ComponentAddress;
use std::sync::Arc;
use vfs::directory::entry::OpenRequest;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use vfs::ToObjectRequest;
use zx::{self, Resource};
use {
    fidl_fuchsia_boot as fboot, fidl_fuchsia_component_resolution as fresolution,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio,
    fidl_fuchsia_kernel as fkernel, fidl_fuchsia_pkg as fpkg, fidl_fuchsia_process as fprocess,
    fidl_fuchsia_sys2 as fsys, fidl_fuchsia_time as ftime, fuchsia_async as fasync,
};

#[cfg(feature = "tracing")]
use {
    cm_config::TraceProvider,
    fidl::endpoints::{self, Proxy},
    fidl_fuchsia_tracing_provider as ftp,
    zx::AsHandleRef,
};

// Allow shutdown to take up to an hour.
pub static SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

// LINT.IfChange
/// Set the size of the inspect VMO to be 350 KiB.
const INSPECTOR_SIZE: usize = 350 * 1024;
// LINT.ThenChange(/src/tests/diagnostics/meta/component_manager_status_tests.cml)

pub struct BuiltinEnvironmentBuilder {
    // TODO(60804): Make component manager's namespace injectable here.
    runtime_config: Option<RuntimeConfig>,
    top_instance: Option<Arc<ComponentManagerInstance>>,
    bootfs_svc: Option<BootfsSvc>,
    builtin_runners: Vec<BuiltinRunnerData>,
    resolvers: ResolverRegistry,
    utc_clock: Option<Arc<UtcClock>>,
    add_environment_resolvers: bool,
    inspector: Option<Inspector>,
    crash_records: CrashRecords,
    instance_registry: Arc<InstanceRegistry>,
    #[cfg(test)]
    scope_factory: Option<Box<dyn Fn() -> ExecutionScope + Send + Sync + 'static>>,
}

struct BuiltinRunnerData {
    name: Name,
    runner: Arc<dyn BuiltinRunnerFactory>,
    add_to_env: bool,
}

impl Default for BuiltinEnvironmentBuilder {
    fn default() -> Self {
        Self {
            runtime_config: None,
            top_instance: None,
            bootfs_svc: None,
            builtin_runners: vec![],
            resolvers: ResolverRegistry::default(),
            utc_clock: None,
            add_environment_resolvers: false,
            inspector: None,
            crash_records: CrashRecords::new(),
            instance_registry: InstanceRegistry::new(),
            #[cfg(test)]
            scope_factory: None,
        }
    }
}

impl BuiltinEnvironmentBuilder {
    pub fn new() -> Self {
        BuiltinEnvironmentBuilder::default()
    }

    pub fn set_runtime_config(mut self, runtime_config: RuntimeConfig) -> Self {
        assert!(self.runtime_config.is_none());
        let top_instance = Arc::new(ComponentManagerInstance::new(
            runtime_config.namespace_capabilities.clone(),
            runtime_config.builtin_capabilities.clone(),
        ));
        self.runtime_config = Some(runtime_config);
        self.top_instance = Some(top_instance);
        self
    }

    pub fn set_bootfs_svc(mut self, bootfs_svc: BootfsSvc) -> Self {
        self.bootfs_svc = Some(bootfs_svc);
        self
    }

    #[cfg(test)]
    pub fn set_inspector(mut self, inspector: Inspector) -> Self {
        self.inspector = Some(inspector);
        self
    }

    /// Set a custom execution scope on components. This is useful for tests that wish
    /// to directly control the execution of scoped tasks.
    #[cfg(test)]
    pub fn set_scope_factory(
        mut self,
        f: Box<dyn Fn() -> ExecutionScope + Send + Sync + 'static>,
    ) -> Self {
        self.scope_factory = Some(f);
        self
    }

    /// Create a UTC clock if required.
    /// Not every instance of component_manager running on the system maintains a
    /// UTC clock. Only the root component_manager should have the `maintain-utc-clock`
    /// config flag set.
    pub async fn create_utc_clock(mut self, bootfs: &Option<BootfsSvc>) -> Result<Self, Error> {
        let runtime_config = self
            .runtime_config
            .as_ref()
            .ok_or_else(|| format_err!("Runtime config should be set to create utc clock."))?;
        self.utc_clock = if runtime_config.maintain_utc_clock {
            Some(Arc::new(create_utc_clock(&bootfs).await.context("failed to create UTC clock")?))
        } else {
            None
        };
        Ok(self)
    }

    pub fn add_builtin_elf_runner(self, add_to_env: bool) -> Result<Self, Error> {
        use crate::builtin::builtin_runner::{BuiltinRunner, ElfRunnerResources};
        let runtime_config = self
            .runtime_config
            .as_ref()
            .ok_or_else(|| format_err!("Runtime config should be set to add builtin runner."))?;

        let elf_runner_resources = ElfRunnerResources {
            security_policy: runtime_config.security_policy.clone(),
            utc_clock: self.utc_clock.clone(),
            crash_records: self.crash_records.clone(),
            instance_registry: self.instance_registry.clone(),
        };
        let program = BuiltinRunner::get_elf_program(Arc::new(elf_runner_resources));
        self.add_builtin_runner("builtin_elf_runner", program, add_to_env)
    }

    pub fn add_builtin_runner(
        self,
        name: &str,
        program: BuiltinProgramGen,
        add_to_env: bool,
    ) -> Result<Self, Error> {
        use crate::builtin::builtin_runner::BuiltinRunner;

        let top_instance = self.top_instance.clone().unwrap();
        let runner = Arc::new(BuiltinRunner::new(
            fuchsia_runtime::job_default(),
            top_instance.task_group(),
            program,
        ));
        Ok(self.add_runner(name.parse().unwrap(), runner, add_to_env))
    }

    pub fn add_runner(
        mut self,
        name: Name,
        runner: Arc<dyn BuiltinRunnerFactory>,
        add_to_env: bool,
    ) -> Self {
        // We don't wrap these in a BuiltinRunner immediately because that requires the
        // RuntimeConfig, which may be provided after this or may fall back to the default.
        self.builtin_runners.push(BuiltinRunnerData { name, runner, add_to_env });
        self
    }

    #[cfg(test)]
    pub fn add_resolver(
        mut self,
        scheme: String,
        resolver: Arc<dyn Resolver + Send + Sync + 'static>,
    ) -> Self {
        self.resolvers.register(scheme, resolver);
        self
    }

    /// Adds standard resolvers whose dependencies are available in the process's namespace and for
    /// whose scheme no resolver is registered through `add_resolver` by the time `build()` is
    /// is called. This includes:
    ///   - A fuchsia-boot resolver if /boot is available.
    pub fn include_namespace_resolvers(mut self) -> Self {
        self.add_environment_resolvers = true;
        self
    }

    pub async fn build(mut self) -> Result<BuiltinEnvironment, Error> {
        let runtime_config = self
            .runtime_config
            .ok_or_else(|| format_err!("Runtime config is required for BuiltinEnvironment."))?;

        // Drain messages from `fuchsia.boot.Userboot`, and expose appropriate capabilities.
        let userboot = take_startup_handle(HandleInfo::new(HandleType::User0, 0))
            .map(zx::Channel::from)
            .map(fasync::Channel::from_channel)
            .map(fboot::UserbootRequestStream::from_channel);

        let mut svc_stash_provider = None;
        let mut bootfs_entries = Vec::new();
        if let Some(userboot) = userboot {
            let messages = userboot.try_collect::<Vec<UserbootRequest>>().await;

            if let Ok(mut messages) = messages {
                while let Some(request) = messages.pop() {
                    match request {
                        UserbootRequest::PostStashSvc { stash_svc_endpoint, control_handle: _ } => {
                            if svc_stash_provider.is_some() {
                                warn!("Expected at most a single SvcStash, but more were found. Last entry will be preserved.");
                            }
                            svc_stash_provider =
                                Some(SvcStashCapability::new(stash_svc_endpoint.into_channel()));
                        }
                        UserbootRequest::PostBootfsFiles { files, control_handle: _ } => {
                            bootfs_entries.extend(files);
                        }
                    }
                }
            } else if let Err(err) = messages {
                error!("Error extracting 'fuchsia.boot.Userboot' messages: {err}");
            }
        }

        let system_resource_handle =
            take_startup_handle(HandleType::SystemResource.into()).map(zx::Resource::from);
        if let Some(bootfs_svc) = self.bootfs_svc {
            // Set up the Rust bootfs VFS, and bind to the '/boot' namespace. This should
            // happen as early as possible when building the component manager as other objects
            // may require reading from '/boot' for configuration, etc.
            let bootfs_svc = match runtime_config.vmex_source {
                VmexSource::SystemResource => bootfs_svc
                    .ingest_bootfs_vmo_with_system_resource(
                        &system_resource_handle,
                        bootfs_entries,
                    )?
                    .publish_kernel_vmo(get_stable_vdso_vmo()?)?
                    .publish_kernel_vmo(get_next_vdso_vmo()?)?
                    .publish_kernel_vmo(get_vdso_vmo(&zx::Name::new_lossy("vdso/test1"))?)?
                    .publish_kernel_vmo(get_vdso_vmo(&zx::Name::new_lossy("vdso/test2"))?)?
                    .publish_kernel_vmos(HandleType::KernelFileVmo, 0)?,
                VmexSource::Namespace => {
                    let mut bootfs_svc =
                        bootfs_svc.ingest_bootfs_vmo_with_namespace_vmex(bootfs_entries).await?;
                    // This is a nested component_manager - tolerate missing vdso's.
                    for kernel_vmo in [
                        get_stable_vdso_vmo(),
                        get_next_vdso_vmo(),
                        get_vdso_vmo(&zx::Name::new_lossy("vdso/test1")),
                        get_vdso_vmo(&zx::Name::new_lossy("vdso/test2")),
                    ]
                    .into_iter()
                    .filter_map(|v| v.ok())
                    {
                        bootfs_svc = bootfs_svc.publish_kernel_vmo(kernel_vmo)?;
                    }
                    bootfs_svc.publish_kernel_vmos(HandleType::KernelFileVmo, 0)?
                }
            };
            bootfs_svc.create_and_bind_vfs()?;
        }

        let root_component_url = match runtime_config.root_component_url.as_ref() {
            Some(url) => url.clone(),
            None => {
                return Err(format_err!("Root component url is required from RuntimeConfig."));
            }
        };

        register_builtin_resolver(&mut self.resolvers);

        let boot_resolvers = if self.add_environment_resolvers {
            register_boot_resolver(&mut self.resolvers, &runtime_config).await?
        } else {
            None
        };

        let realm_builder_resolver = match runtime_config.realm_builder_resolver_and_runner {
            fidl_fuchsia_component_internal::RealmBuilderResolverAndRunner::Namespace => {
                self.builtin_runners.push(BuiltinRunnerData {
                    name: REALM_BUILDER_RUNNER_NAME.parse().unwrap(),
                    runner: Arc::new(RealmBuilderRunnerFactory::new()),
                    add_to_env: true,
                });
                Some(register_realm_builder_resolver(&mut self.resolvers)?)
            }
            fidl_fuchsia_component_internal::RealmBuilderResolverAndRunner::None => None,
        };

        let capability_passthrough = match runtime_config.realm_builder_resolver_and_runner {
            fidl_fuchsia_component_internal::RealmBuilderResolverAndRunner::Namespace => true,
            fidl_fuchsia_component_internal::RealmBuilderResolverAndRunner::None => false,
        };

        let runner_map = self
            .builtin_runners
            .iter()
            .filter_map(|data| {
                let BuiltinRunnerData { name, runner: _, add_to_env } = data;
                if !add_to_env {
                    return None;
                }
                Some((
                    name.clone(),
                    RunnerRegistration {
                        source_name: name.clone(),
                        target_name: name.clone(),
                        source: cm_rust::RegistrationSource::Self_,
                    },
                ))
            })
            .collect();

        let runtime_config = Arc::new(runtime_config);

        let top_instance = self.top_instance.unwrap().clone();
        let params = ModelParams {
            root_component_url,
            root_environment: Environment::new_root(
                &top_instance,
                RunnerRegistry::new(runner_map),
                self.resolvers,
                DebugRegistry::default(),
            ),
            runtime_config: Arc::clone(&runtime_config),
            top_instance,
            instance_registry: self.instance_registry,
            #[cfg(test)]
            scope_factory: self.scope_factory,
        };

        // Wrap BuiltinRunnerFactory in BuiltinRunner now that we have the definite RuntimeConfig.
        let builtin_runners = self
            .builtin_runners
            .into_iter()
            .map(|data| {
                let BuiltinRunnerData { name, runner, add_to_env } = data;
                BuiltinRunner::new(name, runner, runtime_config.security_policy.clone(), add_to_env)
            })
            .collect();

        Ok(BuiltinEnvironment::new(
            params,
            runtime_config,
            system_resource_handle,
            builtin_runners,
            boot_resolvers,
            realm_builder_resolver,
            self.utc_clock,
            self.inspector
                .unwrap_or_else(|| component::init_inspector_with_size(INSPECTOR_SIZE).clone()),
            self.crash_records,
            capability_passthrough,
            svc_stash_provider,
        )
        .await?)
    }
}

/// Constructs a [ComponentInput] that contains built-in capabilities.
pub struct RootComponentInputBuilder {
    input: ComponentInput,
    task_group: WeakTaskGroup,
    security_policy: Arc<SecurityPolicy>,
    policy_checker: GlobalPolicyChecker,
    builtin_capabilities: Vec<cm_rust::CapabilityDecl>,
}

impl RootComponentInputBuilder {
    pub fn new(task_group: WeakTaskGroup, runtime_config: &Arc<RuntimeConfig>) -> Self {
        Self {
            input: ComponentInput::default(),
            task_group,
            security_policy: runtime_config.security_policy.clone(),
            policy_checker: GlobalPolicyChecker::new(runtime_config.security_policy.clone()),
            builtin_capabilities: runtime_config.builtin_capabilities.clone(),
        }
    }

    /// Adds a new builtin protocol to the input that will be given to the root component. If the
    /// protocol is not listed in `self.builtin_capabilities`, then it will silently be omitted
    /// from the input.
    fn add_builtin_protocol_if_enabled<P>(
        &mut self,
        task_to_launch: impl Fn(P::RequestStream) -> BoxFuture<'static, Result<(), anyhow::Error>>
            + Sync
            + Send
            + 'static,
    ) where
        P: DiscoverableProtocolMarker + ProtocolMarker,
    {
        let name = Name::new(P::PROTOCOL_NAME).unwrap();
        self.add_named_builtin_protocol_if_enabled::<P>(name, task_to_launch)
    }

    /// Adds a new builtin protocol to the input that will be given to the root component. If the
    /// protocol is not listed in `self.builtin_capabilities`, then it will silently be omitted
    /// from the input.
    /// The protocol's name, which is the value checked for in `self.builtin_capabilities` and how
    /// the protocol is exposed to the root component, will be `name` instead of `P::PROTOCOL_NAME`.
    fn add_named_builtin_protocol_if_enabled<P>(
        &mut self,
        name: cm_types::Name,
        task_to_launch: impl Fn(P::RequestStream) -> BoxFuture<'static, Result<(), anyhow::Error>>
            + Sync
            + Send
            + 'static,
    ) where
        P: DiscoverableProtocolMarker + ProtocolMarker,
    {
        // TODO: check capability type too
        // TODO: if we store the capabilities in a hashmap by name, then we can remove them as
        // they're added and confirm at the end that we've not been asked to enable something
        // unknown.
        if self.builtin_capabilities.iter().find(|decl| decl.name() == &name).is_none() {
            // This builtin protocol is not enabled based on the runtime config, so don't add the
            // capability to the input.
            return;
        }

        let capability_source = CapabilitySource::Builtin(BuiltinSource {
            capability: InternalCapability::Protocol(name.clone()),
        });

        let launch = LaunchTaskOnReceive::new(
            capability_source,
            self.task_group.clone(),
            name.clone(),
            Some(self.policy_checker.clone()),
            Arc::new(move |server_end, _| {
                task_to_launch(crate::sandbox_util::take_handle_as_stream::<P>(server_end)).boxed()
            }),
        );

        match self.input.insert_capability(
            &name,
            launch
                .into_router()
                .with_porcelain_type(
                    CapabilityTypeName::Protocol,
                    ExtendedMoniker::ComponentManager,
                )
                .into(),
        ) {
            Ok(()) => (),
            Err(e) => warn!("failed to add {name} to root component input: {e:?}"),
        }
    }

    fn add_namespace_protocol(&mut self, protocol: &cm_rust::ProtocolDecl) {
        let path = protocol.source_path.as_ref().unwrap().to_string();
        let capability_source = CapabilitySource::Namespace(NamespaceSource {
            capability: ComponentCapability::Protocol(protocol.clone()),
        });
        let launch = LaunchTaskOnReceive::new(
            capability_source,
            self.task_group.clone(),
            "namespace capability dispatcher",
            Some(self.policy_checker.clone()),
            Arc::new(move |server_end, _| {
                let path = path.clone();
                let fut = async move {
                    fuchsia_fs::node::open_channel_in_namespace(
                        &path,
                        fio::Flags::empty(),
                        ServerEnd::new(server_end),
                    )
                    .map_err(|e| {
                        warn!(
                            "failed to open capability in component_manager's namespace \
                    \"{path}\": {e}"
                        );
                        format_err!("{e:?}")
                    })
                };
                fut.boxed()
            }),
        );
        match self.input.insert_capability(
            &protocol.name,
            launch
                .into_router()
                .with_porcelain_type(
                    CapabilityTypeName::Protocol,
                    ExtendedMoniker::ComponentManager,
                )
                .into(),
        ) {
            Ok(()) => (),
            Err(e) => warn!("failed to add {} to root component input: {e:?}", protocol.name),
        }
    }

    pub fn add_resolver(
        &mut self,
        resolver_schema: String,
        resolver: Arc<dyn Resolver + Send + Sync + 'static>,
    ) {
        let resolver_schema = Name::new(resolver_schema)
            .expect("invalid resolver schema, this should be prevented by manifest_validation");
        let capability_source = CapabilitySource::Builtin(BuiltinSource {
            capability: InternalCapability::Resolver(resolver_schema.clone()),
        });
        let resolver = Arc::new(resolver);
        async fn do_resolve(
            weak_target: &WeakComponentInstance,
            resolver: &Arc<dyn Resolver + Send + Sync>,
            url: String,
            context: Option<fresolution::Context>,
        ) -> Result<fresolution::Component, fresolution::ResolverError> {
            let target = weak_target.upgrade().map_err(|_| fresolution::ResolverError::Internal)?;
            let url = Url::new(url).map_err(|_| fresolution::ResolverError::InvalidArgs)?;
            let component_address = match context {
                Some(context) => {
                    ComponentAddress::from_url_and_context(&url, context.into(), &target).await
                }
                None => ComponentAddress::from_url(&url, &target).await,
            }
            .map_err(|_| fresolution::ResolverError::InvalidArgs)?;
            let component = resolver.resolve(&component_address).await?;
            Ok(component.into())
        }
        let name_for_warn = resolver_schema.clone();
        let launch = LaunchTaskOnReceive::new(
            capability_source,
            self.task_group.clone(),
            resolver_schema.clone(),
            Some(self.policy_checker.clone()),
            Arc::new(move |server_end, weak_target| {
                let resolver = resolver.clone();
                let name_for_warn = name_for_warn.clone();
                async move {
                    let mut stream =
                        take_handle_as_stream::<fresolution::ResolverMarker>(server_end);
                    while let Some(request) = stream.try_next().await? {
                        match request {
                            fresolution::ResolverRequest::Resolve { component_url, responder } => {
                                responder.send(
                                    do_resolve(&weak_target, &resolver, component_url, None).await,
                                )?;
                            }
                            fresolution::ResolverRequest::ResolveWithContext {
                                component_url,
                                context,
                                responder,
                            } => {
                                responder.send(
                                    do_resolve(
                                        &weak_target,
                                        &resolver,
                                        component_url,
                                        Some(context),
                                    )
                                    .await,
                                )?;
                            }
                            other_request => warn!(
                                "unexpected resolver request received for resolver {}: {:?}",
                                name_for_warn, other_request
                            ),
                        };
                    }
                    Ok(())
                }
                .boxed()
            }),
        );
        // TODO(https://fxbug.dev/369573212): Historically the fuchsia-boot resolver has been
        // placed in the root component's environment as `fuchsia-boot` and offered to the root
        // component as `boot_resolver`. This discrepancy must be handled here, as existing tests
        // and production manifests expect this
        // behavior.
        let resolver_name_str = match resolver_schema.as_str() {
            "fuchsia-boot" => "boot_resolver".to_string(),
            resolver_name => resolver_name.to_string(),
        };
        let resolver_name = Name::new(resolver_name_str)
            .expect("invalid resolver name, this should be prevented by manifest_validation");

        let r = launch
            .into_router()
            .with_porcelain_type(CapabilityTypeName::Resolver, ExtendedMoniker::ComponentManager);
        if let Err(e) =
            self.input.capabilities().insert_capability(&resolver_name, r.clone().into())
        {
            warn!(
                "failed to add resolver {} to root component offered capabilities: {e:?}",
                resolver_name
            );
        }
        if let Err(e) =
            self.input.environment().resolvers().insert_capability(&resolver_schema, r.into())
        {
            warn!("failed to add resolver {} to root component environment: {e:?}", resolver_schema)
        }
    }

    fn add_runner_if_enabled(&mut self, runner: BuiltinRunner) {
        if self.builtin_capabilities.iter().find(|decl| decl.name() == runner.name()).is_none() {
            // This builtin protocol is not enabled based on the runtime config, so don't add the
            // capability to the input.
            return;
        }
        self.add_runner(runner);
    }

    pub fn add_runner(&mut self, runner: BuiltinRunner) {
        let name = runner.name().clone();
        let add_to_env = runner.add_to_env();
        let capability_source = CapabilitySource::Builtin(BuiltinSource {
            capability: InternalCapability::Runner(name.clone()),
        });
        let security_policy = self.security_policy.clone();
        let execution_scope = ExecutionScope::new();
        let launch = LaunchTaskOnReceive::new(
            capability_source,
            self.task_group.clone(),
            runner.name().clone(),
            Some(self.policy_checker.clone()),
            Arc::new(move |server_end, weak_component| {
                let flags = fio::OpenFlags::empty();
                let mut object_request = flags.to_object_request(server_end);
                runner
                    .factory()
                    .clone()
                    .get_scoped_runner(
                        ScopedPolicyChecker::new(security_policy.clone(), weak_component.moniker),
                        OpenRequest::new(
                            execution_scope.clone(),
                            flags,
                            Path::dot(),
                            &mut object_request,
                        ),
                    )
                    .expect("TODO");
                future::ready(Ok(())).boxed()
            }),
        );
        let r = launch.into_router();
        if let Err(e) = self.input.capabilities().insert_capability(
            &name,
            r.clone()
                .with_porcelain_type(CapabilityTypeName::Runner, ExtendedMoniker::ComponentManager)
                .into(),
        ) {
            warn!("failed to add runner {} to root component offered capabilities: {e:?}", name);
        }
        if add_to_env {
            if let Err(e) = self.input.environment().runners().insert_capability(
                &name,
                r.with_porcelain_type(
                    CapabilityTypeName::Runner,
                    ExtendedMoniker::ComponentManager,
                )
                .into(),
            ) {
                warn!("failed to add runner {} to root component environment: {e:?}", name)
            }
        }
    }

    pub fn build(self) -> ComponentInput {
        self.input
    }
}

/// The built-in environment consists of the set of the root services and framework services. Use
/// BuiltinEnvironmentBuilder to construct one.
///
/// The available built-in capabilities depends on the configuration provided in Arguments:
/// * If [RuntimeConfig::use_builtin_process_launcher] is true, a fuchsia.process.Launcher service
///   is available.
/// * If [RuntimeConfig::maintain_utc_clock] is true, a fuchsia.time.Maintenance service is
///   available.
pub struct BuiltinEnvironment {
    pub model: Arc<Model>,

    pub config_override: Option<ConfigOverride>,
    pub realm_query: Option<RealmQuery>,
    pub lifecycle_controller: Option<LifecycleController>,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub event_registry: Arc<EventRegistry>,
    pub event_source_factory: Arc<EventSourceFactory>,
    pub capability_store: CapabilityStore,
    pub stop_notifier: Arc<RootStopNotifier>,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub inspect_sink_provider: Arc<InspectSinkProvider>,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub event_stream_provider: Arc<EventStreamProvider>,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub event_logger: Option<Arc<EventLogger>>,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub component_tree_stats: Arc<ComponentTreeStats<DiagnosticsTask>>,
    // Keeps the inspect node alive.
    _component_lifecycle_time_stats: Arc<ComponentLifecycleTimeStats>,
    // Keeps the inspect node alive.
    _component_escrow_duration_status: Arc<::diagnostics::escrow::DurationStats>,
    pub debug: bool,
    // Where to look for the trace provider
    #[cfg(feature = "tracing")]
    pub trace_provider: TraceProvider,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub num_threads: usize,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub realm_builder_resolver: Option<RealmBuilderResolver>,
    capability_passthrough: bool,
    _service_fs_task: Option<fasync::Task<()>>,
}

impl BuiltinEnvironment {
    async fn new(
        mut params: ModelParams,
        runtime_config: Arc<RuntimeConfig>,
        system_resource_handle: Option<Resource>,
        builtin_runners: Vec<BuiltinRunner>,
        boot_resolvers: Option<(FuchsiaBootResolver, Option<Arc<FuchsiaBootPackageResolver>>)>,
        realm_builder_resolver: Option<RealmBuilderResolver>,
        utc_clock: Option<Arc<UtcClock>>,
        inspector: Inspector,
        crash_records: CrashRecords,
        capability_passthrough: bool,
        svc_stash_provider: Option<Arc<SvcStashCapability>>,
    ) -> Result<BuiltinEnvironment, Error> {
        let debug = runtime_config.debug;
        #[cfg(feature = "tracing")]
        let trace_provider = runtime_config.trace_provider.clone();

        let num_threads = runtime_config.num_threads.clone();
        let top_instance = params.top_instance.clone();

        let mut root_input_builder =
            RootComponentInputBuilder::new(top_instance.task_group().as_weak(), &runtime_config);

        for (resolver_schema, resolver) in params.root_environment.drain_resolvers() {
            root_input_builder.add_resolver(resolver_schema, resolver);
        }

        // If capability passthrough is enabled, add capabilities offered from
        // the parent to the input dictionary of the root component.
        if capability_passthrough {
            match fuchsia_fs::directory::open_in_namespace("/parent-offered", fio::Flags::empty()) {
                Ok(passthrough_dir) => match fuchsia_fs::directory::readdir(&passthrough_dir).await
                {
                    Ok(entries) => {
                        for entry in entries {
                            root_input_builder.add_namespace_protocol(&cm_rust::ProtocolDecl {
                                name: cm_types::BoundedName::new(&entry.name).unwrap(),
                                source_path: Some(
                                    cm_types::Path::new(format!("/parent-offered/{}", &entry.name))
                                        .unwrap(),
                                ),
                                delivery: cm_types::DeliveryType::Immediate,
                            });
                        }
                    }
                    Err(e) => log::warn!("failed to read entries in /parent-offered: {e}"),
                },
                Err(e) => {
                    log::warn!("failed to open /parent-offered dir: {e}");
                }
            }
        }

        for namespace_capability in top_instance.namespace_capabilities() {
            match namespace_capability {
                cm_rust::CapabilityDecl::Protocol(p) => {
                    root_input_builder.add_namespace_protocol(&p);
                }
                _ => {
                    // Bedrock doesn't support these capability types yet, they'll fall back to
                    // legacy routing
                }
            }
        }

        // Extracted from userboot protocol in environment builder.
        if let Some(svc_stash_provider) = svc_stash_provider {
            root_input_builder.add_builtin_protocol_if_enabled::<fboot::SvcStashProviderMarker>(
                move |stream| svc_stash_provider.clone().serve(stream).boxed(),
            );
        }

        // Set up ProcessLauncher if available.
        if runtime_config.use_builtin_process_launcher {
            root_input_builder.add_builtin_protocol_if_enabled::<fprocess::LauncherMarker>(
                |stream| {
                    async move {
                        ProcessLauncher::serve(stream).await.map_err(|e| format_err!("{:?}", e))
                    }
                    .boxed()
                },
            );
        }

        // Set up RootJob service.
        root_input_builder.add_builtin_protocol_if_enabled::<fkernel::RootJobMarker>(|stream| {
            RootJob::serve(stream, zx::Rights::SAME_RIGHTS).boxed()
        });

        // Set up RootJobForInspect service.
        root_input_builder.add_builtin_protocol_if_enabled::<fkernel::RootJobForInspectMarker>(
            |stream| {
                let stream = stream.cast_stream::<fkernel::RootJobRequestStream>();
                let rights = zx::Rights::INSPECT
                    | zx::Rights::ENUMERATE
                    | zx::Rights::DUPLICATE
                    | zx::Rights::TRANSFER
                    | zx::Rights::GET_PROPERTY;
                RootJob::serve(stream, rights).boxed()
            },
        );

        let mmio_resource_handle =
            take_startup_handle(HandleType::MmioResource.into()).map(zx::Resource::from);

        let irq_resource_handle =
            take_startup_handle(HandleType::IrqResource.into()).map(zx::Resource::from);

        let zbi_vmo_handle = take_startup_handle(HandleType::BootdataVmo.into()).map(zx::Vmo::from);
        let mut zbi_parser = match zbi_vmo_handle {
            Some(zbi_vmo) => Some(
                ZbiParser::new(zbi_vmo)
                    .set_store_item(ZbiType::Cmdline)
                    .set_store_item(ZbiType::ImageArgs)
                    .set_store_item(ZbiType::Crashlog)
                    .set_store_item(ZbiType::KernelDriver)
                    .set_store_item(ZbiType::PlatformId)
                    .set_store_item(ZbiType::StorageBootfsFactory)
                    .set_store_item(ZbiType::StorageRamdisk)
                    .set_store_item(ZbiType::SerialNumber)
                    .set_store_item(ZbiType::BootloaderFile)
                    .set_store_item(ZbiType::DeviceTree)
                    .set_store_item(ZbiType::DriverMetadata)
                    .set_store_item(ZbiType::CpuTopology)
                    .set_store_item(ZbiType::AcpiRsdp)
                    .set_store_item(ZbiType::Smbios)
                    .set_store_item(ZbiType::Framebuffer)
                    .parse()?,
            ),
            None => None,
        };

        // Set up BootArguments service.
        let boot_args = BootArguments::new(&mut zbi_parser).await?;
        root_input_builder.add_builtin_protocol_if_enabled::<fboot::ArgumentsMarker>(
            move |stream| boot_args.clone().serve(stream).boxed(),
        );

        if let Some(mut zbi_parser) = zbi_parser {
            let factory_items = FactoryItems::new(&mut zbi_parser)?;
            root_input_builder.add_builtin_protocol_if_enabled::<fboot::FactoryItemsMarker>(
                move |stream| factory_items.clone().serve(stream).boxed(),
            );

            let items = Items::new(zbi_parser)?;
            root_input_builder.add_builtin_protocol_if_enabled::<fboot::ItemsMarker>(
                move |stream| items.clone().serve(stream).boxed(),
            );
        }

        // Set up CrashRecords service.
        let crash_records_svc = CrashIntrospectSvc::new(crash_records);
        root_input_builder.add_builtin_protocol_if_enabled::<fsys::CrashIntrospectMarker>(
            move |stream| crash_records_svc.clone().serve(stream).boxed(),
        );

        // Set up KernelStats service.
        let info_resource_handle = system_resource_handle
            .as_ref()
            .map(|handle| {
                match handle.create_child(
                    zx::ResourceKind::SYSTEM,
                    None,
                    zx::sys::ZX_RSRC_SYSTEM_INFO_BASE,
                    1,
                    b"info",
                ) {
                    Ok(resource) => Some(resource),
                    Err(_) => None,
                }
            })
            .flatten();
        if let Some(kernel_stats) = info_resource_handle.map(KernelStats::new) {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::StatsMarker>(
                move |stream| kernel_stats.clone().serve(stream).boxed(),
            );
        }

        // Set up the ReadOnlyLog service.
        let debuglog_resource = system_resource_handle
            .as_ref()
            .map(|handle| {
                match handle.create_child(
                    zx::ResourceKind::SYSTEM,
                    None,
                    zx::sys::ZX_RSRC_SYSTEM_DEBUGLOG_BASE,
                    1,
                    b"debuglog",
                ) {
                    Ok(resource) => Some(resource),
                    Err(_) => None,
                }
            })
            .flatten();

        if let Some(debuglog_resource) = debuglog_resource {
            let read_only_log = ReadOnlyLog::new(debuglog_resource);

            root_input_builder.add_builtin_protocol_if_enabled::<fboot::ReadOnlyLogMarker>(
                move |stream| read_only_log.clone().serve(stream).boxed(),
            );
        }

        // Set up WriteOnlyLog service.
        let debuglog_resource = system_resource_handle
            .as_ref()
            .map(|handle| {
                match handle.create_child(
                    zx::ResourceKind::SYSTEM,
                    None,
                    zx::sys::ZX_RSRC_SYSTEM_DEBUGLOG_BASE,
                    1,
                    b"debuglog",
                ) {
                    Ok(resource) => Some(resource),
                    Err(_) => None,
                }
            })
            .flatten();

        if let Some(debuglog_resource) = debuglog_resource {
            let write_only_log = WriteOnlyLog::new(
                zx::DebugLog::create(&debuglog_resource, zx::DebugLogOpts::empty()).unwrap(),
            );

            root_input_builder.add_builtin_protocol_if_enabled::<fboot::WriteOnlyLogMarker>(
                move |stream| write_only_log.clone().serve(stream).boxed(),
            );
        }

        // Register the UTC time maintainer.
        if let Some(clock) = utc_clock {
            let utc_time_maintainer = Arc::new(UtcInstantMaintainer::new(clock));
            root_input_builder.add_builtin_protocol_if_enabled::<ftime::MaintenanceMarker>(
                move |stream| utc_time_maintainer.clone().serve(stream).boxed(),
            );
        }

        // Set up the MmioResource service.
        let mmio_resource = mmio_resource_handle.map(MmioResource::new);
        if let Some(mmio_resource) = mmio_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::MmioResourceMarker>(
                move |stream| mmio_resource.clone().serve(stream).boxed(),
            );
        }

        #[cfg(target_arch = "x86_64")]
        if let Some(handle) = take_startup_handle(HandleType::IoportResource.into()) {
            let ioport_resource = IoportResource::new(handle.into());
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::IoportResourceMarker>(
                move |stream| ioport_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the IrqResource service.
        let irq_resource = irq_resource_handle.map(IrqResource::new);
        if let Some(irq_resource) = irq_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::IrqResourceMarker>(
                move |stream| irq_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the SMC resource.
        #[cfg(target_arch = "aarch64")]
        if let Some(handle) = take_startup_handle(HandleType::SmcResource.into()) {
            let smc_resource = SmcResource::new(handle.into());
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::SmcResourceMarker>(
                move |stream| smc_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the CpuResource service.
        let cpu_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_CPU_BASE,
                        1,
                        b"cpu",
                    )
                    .ok()
            })
            .map(CpuResource::new)
            .and_then(Result::ok);
        if let Some(cpu_resource) = cpu_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::CpuResourceMarker>(
                move |stream| cpu_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the EnergyInfoResource service.
        let energy_info_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_ENERGY_INFO_BASE,
                        1,
                        b"energy_info",
                    )
                    .ok()
            })
            .map(EnergyInfoResource::new)
            .and_then(Result::ok);
        if let Some(energy_info_resource) = energy_info_resource {
            root_input_builder
                .add_builtin_protocol_if_enabled::<fkernel::EnergyInfoResourceMarker>(
                    move |stream| energy_info_resource.clone().serve(stream).boxed(),
                );
        }

        // Set up the DebugResource service.
        let debug_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_DEBUG_BASE,
                        1,
                        b"debug",
                    )
                    .ok()
            })
            .map(DebugResource::new)
            .and_then(Result::ok);
        if let Some(debug_resource) = debug_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::DebugResourceMarker>(
                move |stream| debug_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the DebuglogResource service.
        let debuglog_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_DEBUGLOG_BASE,
                        1,
                        b"debuglog",
                    )
                    .ok()
            })
            .map(DebuglogResource::new)
            .and_then(Result::ok);
        if let Some(debuglog_resource) = debuglog_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::DebuglogResourceMarker>(
                move |stream| debuglog_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the HypervisorResource service.
        let hypervisor_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_HYPERVISOR_BASE,
                        1,
                        b"hypervisor",
                    )
                    .ok()
            })
            .map(HypervisorResource::new)
            .and_then(Result::ok);
        if let Some(hypervisor_resource) = hypervisor_resource {
            root_input_builder
                .add_builtin_protocol_if_enabled::<fkernel::HypervisorResourceMarker>(
                    move |stream| hypervisor_resource.clone().serve(stream).boxed(),
                );
        }

        // Set up the InfoResource service.
        let info_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_INFO_BASE,
                        1,
                        b"info",
                    )
                    .ok()
            })
            .map(InfoResource::new)
            .and_then(Result::ok);
        if let Some(info_resource) = info_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::InfoResourceMarker>(
                move |stream| info_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the IommuResource service.
        let iommu_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_IOMMU_BASE,
                        1,
                        b"iommu",
                    )
                    .ok()
            })
            .map(IommuResource::new)
            .and_then(Result::ok);
        if let Some(iommu_resource) = iommu_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::IommuResourceMarker>(
                move |stream| iommu_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the MexecResource service.
        let mexec_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_MEXEC_BASE,
                        1,
                        b"mexec",
                    )
                    .ok()
            })
            .map(MexecResource::new)
            .and_then(Result::ok);
        if let Some(mexec_resource) = mexec_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::MexecResourceMarker>(
                move |stream| mexec_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the MsiResource service.
        let msi_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_MSI_BASE,
                        1,
                        b"msi",
                    )
                    .ok()
            })
            .map(MsiResource::new)
            .and_then(Result::ok);
        if let Some(msi_resource) = msi_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::MsiResourceMarker>(
                move |stream| msi_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the PowerResource service.
        let power_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_POWER_BASE,
                        1,
                        b"power",
                    )
                    .ok()
            })
            .map(PowerResource::new)
            .and_then(Result::ok);
        if let Some(power_resource) = power_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::PowerResourceMarker>(
                move |stream| power_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the ProfileResource service.
        let profile_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_PROFILE_BASE,
                        1,
                        b"profile",
                    )
                    .ok()
            })
            .map(ProfileResource::new)
            .and_then(Result::ok);
        if let Some(profile_resource) = profile_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::ProfileResourceMarker>(
                move |stream| profile_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the StallResource service.
        let stall_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_STALL_BASE,
                        1,
                        b"stall",
                    )
                    .ok()
            })
            .map(StallResource::new)
            .and_then(Result::ok);
        if let Some(stall_resource) = stall_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::StallResourceMarker>(
                move |stream| stall_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up the VmexResource service.
        let vmex_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_VMEX_BASE,
                        1,
                        b"vmex",
                    )
                    .ok()
            })
            .map(VmexResource::new)
            .and_then(Result::ok);
        if let Some(vmex_resource) = vmex_resource {
            root_input_builder.add_builtin_protocol_if_enabled::<fkernel::VmexResourceMarker>(
                move |stream| vmex_resource.clone().serve(stream).boxed(),
            );
        }

        // Set up System Controller service.
        let weak_top_instance = Arc::downgrade(&top_instance);
        root_input_builder.add_builtin_protocol_if_enabled::<fsys::SystemControllerMarker>(
            move |stream| {
                SystemController::new(weak_top_instance.clone(), SHUTDOWN_TIMEOUT)
                    .serve(stream)
                    .boxed()
            },
        );

        // Set up the Inspect sink provider.
        let inspect_sink_provider = Arc::new(InspectSinkProvider::new(inspector));

        // Set up the event registry.
        let event_registry = {
            let mut event_registry = EventRegistry::new(Arc::downgrade(&top_instance));
            event_registry.register_synthesis_provider(
                EventType::CapabilityRequested,
                inspect_sink_provider.clone(),
            );
            Arc::new(event_registry)
        };
        let event_stream_provider =
            Arc::new(EventStreamProvider::new(Arc::downgrade(&event_registry)));
        let event_source_factory = EventSourceFactory::new(
            Arc::downgrade(&top_instance),
            Arc::downgrade(&event_registry),
            Arc::downgrade(&event_stream_provider),
        );

        let mut builtin_capabilities: Vec<Box<dyn BuiltinCapability>> =
            vec![Box::new(EventSourceFactoryCapability::new(event_source_factory.clone()))];

        // Set up the boot resolver so it is routable from "above root".
        if let Some((component_resolver, package_resolver)) = boot_resolvers {
            let c = component_resolver.clone();
            root_input_builder.add_builtin_protocol_if_enabled::<fresolution::ResolverMarker>(
                move |stream| {
                    let c = c.clone();
                    async move { c.serve(stream).await.map_err(|e| format_err!("{e:?}")) }.boxed()
                },
            );
            builtin_capabilities.push(Box::new(component_resolver));

            if let Some(package_resolver) = package_resolver {
                root_input_builder
                    .add_named_builtin_protocol_if_enabled::<fpkg::PackageResolverMarker>(
                        cm_types::BoundedName::new("fuchsia.pkg.PackageResolver-boot").unwrap(),
                        move |stream| {
                            let package_resolver = package_resolver.clone();
                            async move {
                                package_resolver
                                    .serve(stream)
                                    .await
                                    .map_err(|e| format_err!("{e:?}"))
                            }
                            .boxed()
                        },
                    );
            }
        }

        for runner in builtin_runners.iter() {
            root_input_builder.add_runner_if_enabled(runner.clone());
        }

        let root_component_input = root_input_builder.build();
        let model = Model::new(params, root_component_input).await?;

        model.root().hooks.install(event_registry.hooks()).await;
        model.root().hooks.install(event_stream_provider.hooks()).await;

        let capability_store = CapabilityStore::new();
        let mut framework_capabilities: Vec<Box<dyn FrameworkCapability>> = vec![
            Box::new(Realm::new(Arc::downgrade(&model), runtime_config.clone())),
            Box::new(IntrospectorFrameworkCapability {
                instance_registry: model.context().instance_registry().clone(),
            }),
            Box::new(BinderFrameworkCapability::new()),
            Box::new(capability_store.clone()),
            Box::new(Namespace::new()),
            Box::new(PkgDirectoryFrameworkCapability::new()),
            Box::new(EventSourceFactoryCapability::new(event_source_factory.clone())),
        ];

        // Set up the builtin runners.
        for runner in builtin_runners {
            builtin_capabilities.push(Box::new(runner));
        }

        let event_logger = if runtime_config.log_all_events {
            let event_logger = Arc::new(EventLogger::new());
            model.root().hooks.install(event_logger.hooks()).await;
            Some(event_logger)
        } else {
            None
        };

        // Set up the root realm stop notifier.
        let stop_notifier = Arc::new(RootStopNotifier::new());
        model.root().hooks.install(stop_notifier.hooks()).await;

        let realm_query = if runtime_config.enable_introspection {
            let cap = RealmQuery::new(Arc::downgrade(&model));
            framework_capabilities.push(Box::new(cap.clone()));
            Some(cap)
        } else {
            None
        };

        let lifecycle_controller = if runtime_config.enable_introspection {
            let cap = LifecycleController::new(Arc::downgrade(&model));
            framework_capabilities.push(Box::new(cap.clone()));
            Some(cap)
        } else {
            None
        };

        let config_override = if runtime_config.enable_introspection {
            let cap = ConfigOverride::new(Arc::downgrade(&model));
            framework_capabilities.push(Box::new(cap.clone()));
            Some(cap)
        } else {
            None
        };

        if runtime_config.enable_introspection {
            framework_capabilities
                .push(Box::new(RouteValidatorFrameworkCapability::new(Arc::downgrade(&model))));
        }

        model
            .context()
            .init_internal_capabilities(builtin_capabilities, framework_capabilities)
            .await;

        // Set up the Component Tree Diagnostics runtime statistics.
        let inspector = inspect_sink_provider.inspector();
        let component_tree_stats =
            ComponentTreeStats::new(inspector.root().create_child("stats")).await;
        component_tree_stats.track_component_manager_stats().await;
        component_tree_stats.start_measuring().await;
        model.root().hooks.install(component_tree_stats.hooks()).await;

        let component_lifecycle_time_stats =
            Arc::new(ComponentLifecycleTimeStats::new(inspector.root().create_child("lifecycle")));
        model.root().hooks.install(component_lifecycle_time_stats.hooks()).await;

        let component_escrow_duration_status = Arc::new(::diagnostics::escrow::DurationStats::new(
            inspector.root().create_child("escrow"),
        ));
        model.root().hooks.install(component_escrow_duration_status.hooks()).await;

        // Serve stats about inspect in a lazy node.
        inspector.record_lazy_stats();

        Ok(BuiltinEnvironment {
            model,
            config_override,
            realm_query,
            lifecycle_controller,
            event_registry,
            event_source_factory,
            capability_store,
            stop_notifier,
            inspect_sink_provider,
            event_stream_provider,
            event_logger,
            component_tree_stats,
            _component_lifecycle_time_stats: component_lifecycle_time_stats,
            _component_escrow_duration_status: component_escrow_duration_status,
            debug,
            #[cfg(feature = "tracing")]
            trace_provider,
            num_threads,
            realm_builder_resolver,
            capability_passthrough,
            _service_fs_task: None,
        })
    }

    /// Returns a ServiceFs that contains protocols served by component manager.
    async fn create_service_fs<'a>(&self) -> Result<ServiceFs<ServiceObj<'a, ()>>, Error> {
        // Create the ServiceFs
        let mut service_fs = ServiceFs::new();

        self.add_exposed_framework_protocol::<_, fsys::ConfigOverrideMarker>(
            &mut service_fs,
            self.config_override.as_ref(),
        );
        self.add_exposed_framework_protocol::<_, fsys::LifecycleControllerMarker>(
            &mut service_fs,
            self.lifecycle_controller.as_ref(),
        );
        self.add_exposed_framework_protocol::<_, fsys::RealmQueryMarker>(
            &mut service_fs,
            self.realm_query.as_ref(),
        );
        self.add_exposed_framework_protocol::<_, fsandbox::CapabilityStoreMarker>(
            &mut service_fs,
            Some(&self.capability_store),
        );

        let scope = self.model.top_instance().task_group();

        // If capability passthrough is enabled, add a remote directory to proxy
        // capabilities exposed by the root component.
        if self.capability_passthrough {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            service_fs.add_remote("root-exposed", proxy);
            let root = self.model.top_instance().root();
            let root = WeakComponentInstance::new(&root);
            scope.spawn(async move {
                let flags = routing::rights::Rights::from(fio::RW_STAR_DIR).into_legacy();
                let mut object_request = flags.to_object_request(server_end);
                object_request.wait_till_ready().await;
                if let Ok(root) = root.upgrade() {
                    root.lock_resolved_state()
                        .await
                        .expect("failed to resolve root component state");
                    root.open_exposed(OpenRequest::new(
                        root.execution_scope.clone(),
                        flags,
                        Path::dot(),
                        &mut object_request,
                    ))
                    .await
                    .expect("unable to open root exposed dir");
                }
            });
        }

        // If component manager is in debug mode, create an event source scoped at the
        // root and offer it via ServiceFs to the outside world.
        if self.debug {
            let event_source = self.event_source_factory.create_for_above_root();

            service_fs.dir("svc").add_fidl_service(move |stream| {
                let mut event_source = event_source.clone();
                // Spawn a short-lived task that adds the EventSource serve to
                // component manager's task scope.
                fasync::Task::spawn(async move {
                    serve_event_stream(
                        event_source
                            .subscribe(vec![
                                EventSubscription {
                                    event_name: UseEventStreamDecl {
                                        source_name: EventType::Started.into(),
                                        source: UseSource::Parent,
                                        scope: None,
                                        target_path: "/svc/fuchsia.component.EventStream"
                                            .parse()
                                            .unwrap(),
                                        filter: None,
                                        availability: Availability::Required,
                                    },
                                },
                                EventSubscription {
                                    event_name: UseEventStreamDecl {
                                        source_name: EventType::Stopped.into(),
                                        source: UseSource::Parent,
                                        scope: None,
                                        target_path: "/svc/fuchsia.component.EventStream"
                                            .parse()
                                            .unwrap(),
                                        filter: None,
                                        availability: Availability::Required,
                                    },
                                },
                                EventSubscription {
                                    event_name: UseEventStreamDecl {
                                        source_name: EventType::Destroyed.into(),
                                        source: UseSource::Parent,
                                        scope: None,
                                        target_path: "/svc/fuchsia.component.EventStream"
                                            .parse()
                                            .unwrap(),
                                        filter: None,
                                        availability: Availability::Required,
                                    },
                                },
                                EventSubscription {
                                    event_name: UseEventStreamDecl {
                                        source_name: EventType::Resolved.into(),
                                        source: UseSource::Parent,
                                        scope: None,
                                        target_path: "/svc/fuchsia.component.EventStream"
                                            .parse()
                                            .unwrap(),
                                        filter: None,
                                        availability: Availability::Required,
                                    },
                                },
                                EventSubscription {
                                    event_name: UseEventStreamDecl {
                                        source_name: EventType::Unresolved.into(),
                                        source: UseSource::Parent,
                                        scope: None,
                                        target_path: "/svc/fuchsia.component.EventStream"
                                            .parse()
                                            .unwrap(),
                                        filter: None,
                                        availability: Availability::Required,
                                    },
                                },
                            ])
                            .await
                            .unwrap(),
                        stream,
                    )
                    .await;
                })
                .detach();
            });
        }

        Ok(service_fs)
    }

    fn add_exposed_framework_protocol<'a, T, M>(
        &self,
        service_fs: &mut ServiceFs<ServiceObj<'a, ()>>,
        cap: Option<&T>,
    ) where
        T: FrameworkCapability + Clone + 'static,
        M: DiscoverableProtocolMarker,
    {
        let Some(cap) = cap else {
            return;
        };
        let cap = cap.clone();
        let scope = self.model.top_instance().task_group();
        let root = self.model.root().as_weak();
        service_fs.dir("svc").add_service_connector(move |server: ServerEnd<M>| {
            let cap = cap.clone();
            let root = root.clone();
            scope.spawn(async move {
                if let Ok(root) = root.upgrade() {
                    if let Err(err) = capability::open_framework(&cap, &root, server.into()).await {
                        warn!(err:%; "Failed to open framework protocol from root {}", M::DEBUG_NAME);
                    }
                }
            });
        });
    }

    /// Bind ServiceFs to a provided channel
    async fn bind_service_fs(
        &mut self,
        channel: fidl::endpoints::ServerEnd<fio::DirectoryMarker>,
    ) -> Result<(), Error> {
        let mut service_fs = self.create_service_fs().await?;

        // Bind to the channel
        service_fs.serve_connection(channel)?;

        // Start up ServiceFs
        self._service_fs_task = Some(fasync::Task::spawn(async move {
            service_fs.collect::<()>().await;
        }));
        Ok(())
    }

    /// Bind ServiceFs to the outgoing directory of this component, if it exists.
    async fn bind_service_fs_to_out(&mut self) -> Result<(), Error> {
        let server_end = match fuchsia_runtime::take_startup_handle(
            fuchsia_runtime::HandleType::DirectoryRequest.into(),
        ) {
            Some(handle) => fidl::endpoints::ServerEnd::new(zx::Channel::from(handle)),
            None => {
                // The component manager running on startup does not get a directory handle. If it was
                // to run as a component itself, it'd get one. When we don't have a handle to the out
                // directory, create one.
                let (_client, server) = fidl::endpoints::create_endpoints();
                server
            }
        };
        self.bind_service_fs(server_end).await
    }

    pub async fn wait_for_root_stop(&self) {
        self.stop_notifier.wait_for_root_stop().await;
    }

    pub async fn run_root(&mut self) -> Result<(), Error> {
        self.bind_service_fs_to_out().await?;

        self.model.start().await;
        component::health().set_ok();
        #[cfg(feature = "tracing")]
        if self.trace_provider == TraceProvider::RootExposed {
            self.connect_to_tracing_from_exposed().await;
        }
        self.wait_for_root_stop().await;

        // Stop serving the out directory, so that more connections to debug capabilities
        // cannot be made.
        drop(self._service_fs_task.take());
        Ok(())
    }

    /// Obtains a connection to tracing, and initializes tracing
    #[cfg(feature = "tracing")]
    async fn connect_to_tracing_from_exposed(&self) {
        let (trace_provider_proxy, server) = endpoints::create_proxy::<ftp::RegistryMarker>();
        let root = self.model.root();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server);
        match root
            .open_exposed(OpenRequest::new(
                root.execution_scope.clone(),
                fio::OpenFlags::empty(),
                ftp::RegistryMarker::PROTOCOL_NAME.try_into().unwrap(),
                &mut object_request,
            ))
            .await
        {
            Ok(()) => {
                fuchsia_trace_provider::trace_provider_create_with_service(
                    trace_provider_proxy.as_channel().raw_handle(),
                );
                fuchsia_trace_provider::trace_provider_wait_for_init();
            }
            Err(e) => info!("Unable to open Registry server for tracing: {}", e),
        }
    }

    #[cfg(test)]
    pub fn inspector(&self) -> &Inspector {
        self.inspect_sink_provider.inspector()
    }
}

fn register_builtin_resolver(resolvers: &mut ResolverRegistry) {
    resolvers.register(BUILTIN_SCHEME.to_string(), Arc::new(BuiltinResolver {}));
}

// Creates a FuchsiaBootResolver if the /boot directory is installed in component_manager's
// namespace, and registers it with the ResolverRegistry. The resolver is returned to so that
// it can be installed as a Builtin capability.
async fn register_boot_resolver(
    resolvers: &mut ResolverRegistry,
    runtime_config: &RuntimeConfig,
) -> Result<Option<(FuchsiaBootResolver, Option<Arc<FuchsiaBootPackageResolver>>)>, Error> {
    let path = match &runtime_config.builtin_boot_resolver {
        BuiltinBootResolver::Boot => "/boot",
        BuiltinBootResolver::None => return Ok(None),
    };
    let resolver =
        FuchsiaBootResolver::new(path).await.context("Failed to create boot resolver")?;
    match resolver {
        None => {
            info!(path:%; "fuchsia-boot resolver unavailable, not in namespace");
            Ok(None)
        }
        Some((component, package)) => {
            resolvers.register(BOOT_SCHEME.into(), Arc::new(component.clone()));
            Ok(Some((component, package)))
        }
    }
}

fn register_realm_builder_resolver(
    resolvers: &mut ResolverRegistry,
) -> Result<RealmBuilderResolver, Error> {
    let resolver =
        RealmBuilderResolver::new().context("Failed to create realm builder resolver")?;
    resolvers.register(REALM_BUILDER_SCHEME.to_string(), Arc::new(resolver.clone()));
    Ok(resolver)
}
