// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_arch = "aarch64")]
use builtins::smc_resource::SmcResource;

use crate::{diagnostics, model::component::WeakComponentInstance};
#[cfg(target_arch = "x86_64")]
use builtins::ioport_resource::IoportResource;

use {
    crate::{
        bootfs::BootfsSvc,
        builtin::{
            builtin_resolver::{BuiltinResolver, SCHEME as BUILTIN_SCHEME},
            crash_introspect::CrashIntrospectSvc,
            fuchsia_boot_resolver::{FuchsiaBootResolverBuiltinCapability, SCHEME as BOOT_SCHEME},
            log::{ReadOnlyLog, WriteOnlyLog},
            realm_builder::{
                RealmBuilderResolver, RealmBuilderRunnerFactory,
                RUNNER_NAME as REALM_BUILDER_RUNNER_NAME, SCHEME as REALM_BUILDER_SCHEME,
            },
            runner::{BuiltinRunner, BuiltinRunnerFactory},
            svc_stash_provider::SvcStashCapability,
            system_controller::SystemController,
            time::{create_utc_clock, UtcTimeMaintainer},
        },
        capability::{BuiltinCapability, CapabilitySource, DerivedCapability, FrameworkCapability},
        diagnostics::{lifecycle::ComponentLifecycleTimeStats, task_metrics::ComponentTreeStats},
        framework::{
            binder::BinderFrameworkCapability,
            factory::{FactoryCapabilityHost, FactoryFrameworkCapability},
            introspector::IntrospectorFrameworkCapability,
            lifecycle_controller::{LifecycleController, LifecycleControllerFrameworkCapability},
            namespace::NamespaceFrameworkCapability,
            pkg_dir::PkgDirectoryFrameworkCapability,
            realm::RealmFrameworkCapability,
            realm_query::{RealmQuery, RealmQueryFrameworkCapability},
            route_validator::RouteValidatorFrameworkCapability,
        },
        inspect_sink_provider::InspectSinkProvider,
        model::events::registry::EventSubscription,
        model::{
            component::manager::ComponentManagerInstance,
            environment::Environment,
            event_logger::EventLogger,
            events::{
                registry::EventRegistry,
                serve::serve_event_stream,
                source_factory::{EventSourceFactory, EventSourceFactoryCapability},
                stream_provider::EventStreamProvider,
            },
            hooks::EventType,
            model::{Model, ModelParams},
            resolver::{box_arc_resolver, ResolverRegistry},
            storage::admin_protocol::StorageAdminDerivedCapability,
            structured_dict::ComponentInput,
            token::InstanceRegistry,
        },
        root_stop_notifier::RootStopNotifier,
        sandbox_util::LaunchTaskOnReceive,
    },
    ::routing::{
        capability_source::{ComponentCapability, InternalCapability},
        component_instance::TopInstanceInterface,
        environment::{DebugRegistry, RunnerRegistry},
        policy::GlobalPolicyChecker,
    },
    anyhow::{format_err, Context as _, Error},
    builtins::{arguments::Arguments as BootArguments, root_job::RootJob},
    builtins::{
        cpu_resource::CpuResource, debug_resource::DebugResource,
        debuglog_resource::DebuglogResource, energy_info_resource::EnergyInfoResource,
        factory_items::FactoryItems, framebuffer_resource::FramebufferResource,
        hypervisor_resource::HypervisorResource, info_resource::InfoResource,
        iommu_resource::IommuResource, irq_resource::IrqResource, items::Items,
        kernel_stats::KernelStats, mexec_resource::MexecResource, mmio_resource::MmioResource,
        msi_resource::MsiResource, power_resource::PowerResource,
        profile_resource::ProfileResource, vmex_resource::VmexResource,
    },
    cm_config::{RuntimeConfig, VmexSource},
    cm_rust::{Availability, RunnerRegistration, UseEventStreamDecl, UseSource},
    cm_types::Name,
    elf_runner::{
        crash_info::CrashRecords,
        process_launcher::ProcessLauncher,
        vdso_vmo::{get_next_vdso_vmo, get_stable_vdso_vmo, get_vdso_vmo},
    },
    fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker, RequestStream, ServerEnd},
    fidl_fuchsia_boot as fboot,
    fidl_fuchsia_component_internal::BuiltinBootResolver,
    fidl_fuchsia_component_resolution as fresolution,
    fidl_fuchsia_diagnostics_types::Task as DiagnosticsTask,
    fidl_fuchsia_io as fio, fidl_fuchsia_kernel as fkernel, fidl_fuchsia_process as fprocess,
    fidl_fuchsia_sys2 as fsys, fidl_fuchsia_time as ftime, fuchsia_async as fasync,
    fuchsia_component::server::*,
    fuchsia_inspect::{component, health::Reporter, stats::InspectorExt, Inspector},
    fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType},
    fuchsia_zbi::{ZbiParser, ZbiType},
    fuchsia_zircon::{self as zx, Clock, Resource},
    futures::{future::BoxFuture, FutureExt, StreamExt},
    moniker::{Moniker, MonikerBase},
    std::sync::Arc,
    tracing::{info, warn},
    vfs::{directory::entry::OpenRequest, path::Path, ToObjectRequest},
};

#[cfg(test)]
use crate::model::resolver::Resolver;

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
    runners: Vec<(Name, Arc<dyn BuiltinRunnerFactory>)>,
    resolvers: ResolverRegistry,
    utc_clock: Option<Arc<Clock>>,
    add_environment_resolvers: bool,
    inspector: Option<Inspector>,
    crash_records: CrashRecords,
    instance_registry: Arc<InstanceRegistry>,
}

impl Default for BuiltinEnvironmentBuilder {
    fn default() -> Self {
        Self {
            runtime_config: None,
            top_instance: None,
            bootfs_svc: None,
            runners: vec![],
            resolvers: ResolverRegistry::default(),
            utc_clock: None,
            add_environment_resolvers: false,
            inspector: None,
            crash_records: CrashRecords::new(),
            instance_registry: InstanceRegistry::new(),
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

    /// Create a UTC clock if required.
    /// Not every instance of component_manager running on the system maintains a
    /// UTC clock. Only the root component_manager should have the `maintain-utc-clock`
    /// config flag set.
    pub async fn create_utc_clock(mut self, bootfs: &Option<BootfsSvc>) -> Result<Self, Error> {
        let runtime_config = self
            .runtime_config
            .as_ref()
            .ok_or(format_err!("Runtime config should be set to create utc clock."))?;
        self.utc_clock = if runtime_config.maintain_utc_clock {
            Some(Arc::new(create_utc_clock(&bootfs).await.context("failed to create UTC clock")?))
        } else {
            None
        };
        Ok(self)
    }

    pub fn add_builtin_runner(self) -> Result<Self, Error> {
        use crate::builtin::builtin_runner::BuiltinRunner;
        use crate::builtin::builtin_runner::ElfRunnerResources;

        let runtime_config = self
            .runtime_config
            .as_ref()
            .ok_or(format_err!("Runtime config should be set to add builtin runner."))?;

        let runner = Arc::new(BuiltinRunner::new(
            self.top_instance.clone().unwrap().task_group(),
            ElfRunnerResources {
                security_policy: runtime_config.security_policy.clone(),
                utc_clock: self.utc_clock.clone(),
                crash_records: self.crash_records.clone(),
                instance_registry: self.instance_registry.clone(),
            },
        ));
        Ok(self.add_runner("builtin".parse().unwrap(), runner))
    }

    pub fn add_runner(mut self, name: Name, runner: Arc<dyn BuiltinRunnerFactory>) -> Self {
        // We don't wrap these in a BuiltinRunner immediately because that requires the
        // RuntimeConfig, which may be provided after this or may fall back to the default.
        self.runners.push((name, runner));
        self
    }

    #[cfg(test)]
    pub fn add_resolver(
        mut self,
        scheme: String,
        resolver: Box<dyn Resolver + Send + Sync + 'static>,
    ) -> Self {
        self.resolvers.register(scheme, resolver);
        self
    }

    /// Adds standard resolvers whose dependencies are available in the process's namespace and for
    /// whose scheme no resolver is registered through `add_resolver` by the time `build()` is
    /// is called. This includes:
    ///   - A fuchsia-boot resolver if /boot is available.
    ///   - A fuchsia-pkg resolver, if /svc/fuchsia.sys.Loader is present.
    ///       - This resolver implementation proxies to that protocol (which is the v1 resolver
    ///         equivalent). This is used for tests or other scenarios where component_manager runs
    ///         as a v1 component.
    pub fn include_namespace_resolvers(mut self) -> Self {
        self.add_environment_resolvers = true;
        self
    }

    pub async fn build(mut self) -> Result<BuiltinEnvironment, Error> {
        let runtime_config = self
            .runtime_config
            .ok_or(format_err!("Runtime config is required for BuiltinEnvironment."))?;

        let system_resource_handle =
            take_startup_handle(HandleType::SystemResource.into()).map(zx::Resource::from);
        if let Some(bootfs_svc) = self.bootfs_svc {
            // Set up the Rust bootfs VFS, and bind to the '/boot' namespace. This should
            // happen as early as possible when building the component manager as other objects
            // may require reading from '/boot' for configuration, etc.
            let bootfs_svc = match runtime_config.vmex_source {
                VmexSource::SystemResource => bootfs_svc
                    .ingest_bootfs_vmo_with_system_resource(&system_resource_handle)?
                    .publish_kernel_vmo(get_stable_vdso_vmo()?)?
                    .publish_kernel_vmo(get_next_vdso_vmo()?)?
                    .publish_kernel_vmo(get_vdso_vmo(c"vdso/test1")?)?
                    .publish_kernel_vmo(get_vdso_vmo(c"vdso/test2")?)?
                    .publish_kernel_vmos(HandleType::KernelFileVmo, 0)?,
                VmexSource::Namespace => {
                    let mut bootfs_svc = bootfs_svc.ingest_bootfs_vmo_with_namespace_vmex().await?;
                    // This is a nested component_manager - tolerate missing vdso's.
                    for kernel_vmo in [
                        get_stable_vdso_vmo(),
                        get_next_vdso_vmo(),
                        get_vdso_vmo(c"vdso/test1"),
                        get_vdso_vmo(c"vdso/test2"),
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

        let boot_resolver = if self.add_environment_resolvers {
            register_boot_resolver(&mut self.resolvers, &runtime_config).await?
        } else {
            None
        };

        let realm_builder_resolver = match runtime_config.realm_builder_resolver_and_runner {
            fidl_fuchsia_component_internal::RealmBuilderResolverAndRunner::Namespace => {
                self.runners.push((
                    REALM_BUILDER_RUNNER_NAME.parse().unwrap(),
                    Arc::new(RealmBuilderRunnerFactory::new()),
                ));
                Some(register_realm_builder_resolver(&mut self.resolvers)?)
            }
            fidl_fuchsia_component_internal::RealmBuilderResolverAndRunner::None => None,
        };

        let capability_passthrough = match runtime_config.realm_builder_resolver_and_runner {
            fidl_fuchsia_component_internal::RealmBuilderResolverAndRunner::Namespace => true,
            fidl_fuchsia_component_internal::RealmBuilderResolverAndRunner::None => false,
        };

        let runner_map = self
            .runners
            .iter()
            .map(|(name, _)| {
                (
                    name.clone(),
                    RunnerRegistration {
                        source_name: name.clone(),
                        target_name: name.clone(),
                        source: cm_rust::RegistrationSource::Self_,
                    },
                )
            })
            .collect();

        let runtime_config = Arc::new(runtime_config);

        let top_instance = self.top_instance.unwrap().clone();
        let params = ModelParams {
            root_component_url: root_component_url.as_str().to_owned(),
            root_environment: Environment::new_root(
                &top_instance,
                RunnerRegistry::new(runner_map),
                self.resolvers,
                DebugRegistry::default(),
            ),
            runtime_config: Arc::clone(&runtime_config),
            top_instance,
        };
        let model = Model::new(params, self.instance_registry).await?;

        // Wrap BuiltinRunnerFactory in BuiltinRunner now that we have the definite RuntimeConfig.
        let builtin_runners = self
            .runners
            .into_iter()
            .map(|(name, runner)| {
                BuiltinRunner::new(name, runner, runtime_config.security_policy.clone())
            })
            .collect();

        Ok(BuiltinEnvironment::new(
            model,
            runtime_config,
            system_resource_handle,
            builtin_runners,
            boot_resolver,
            realm_builder_resolver,
            self.utc_clock,
            self.inspector.unwrap_or(component::init_inspector_with_size(INSPECTOR_SIZE).clone()),
            self.crash_records,
            capability_passthrough,
        )
        .await?)
    }
}

/// Constructs a [ComponentInput] that contains built-in capabilities.
struct RootComponentInputBuilder {
    input: ComponentInput,
    top_instance: Arc<ComponentManagerInstance>,
    policy_checker: GlobalPolicyChecker,
    builtin_capabilities: Vec<cm_rust::CapabilityDecl>,
}

impl RootComponentInputBuilder {
    fn new(
        top_instance: Arc<ComponentManagerInstance>,
        runtime_config: &Arc<RuntimeConfig>,
    ) -> Self {
        Self {
            input: ComponentInput::default(),
            top_instance,
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
        // TODO: check capability type too
        // TODO: if we store the capabilities in a hashmap by name, then we can remove them as
        // they're added and confirm at the end that we've not been asked to enable something
        // unknown.
        if self.builtin_capabilities.iter().find(|decl| decl.name() == &name).is_none() {
            // This builtin protocol is not enabled based on the runtime config, so don't add the
            // capability to the input.
            return;
        }

        let capability_source = CapabilitySource::Builtin {
            capability: InternalCapability::Protocol(name.clone()),
            top_instance: Arc::downgrade(&self.top_instance),
        };

        let launch = LaunchTaskOnReceive::new(
            self.top_instance.task_group().as_weak(),
            name.clone(),
            Some((self.policy_checker.clone(), capability_source)),
            Arc::new(move |server_end, _| {
                task_to_launch(crate::sandbox_util::take_handle_as_stream::<P>(server_end)).boxed()
            }),
        );

        match self.input.insert_capability(
            &P::PROTOCOL_NAME.parse::<Name>().unwrap(),
            launch.into_router().into(),
        ) {
            Ok(()) => (),
            Err(e) => warn!("failed to add {name} to root component input: {e:?}"),
        }
    }

    fn add_namespace_protocol(&mut self, protocol: &cm_rust::ProtocolDecl) {
        let path = protocol.source_path.as_ref().unwrap().to_string();
        let capability_source = CapabilitySource::Namespace {
            capability: ComponentCapability::Protocol(protocol.clone()),
            top_instance: Arc::downgrade(&self.top_instance),
        };
        let launch = LaunchTaskOnReceive::new(
            self.top_instance.task_group().as_weak(),
            "namespace capability dispatcher",
            Some((self.policy_checker.clone(), capability_source)),
            Arc::new(move |server_end, _| {
                let path = path.clone();
                let fut = async move {
                    fuchsia_fs::node::open_channel_in_namespace(
                        &path,
                        fio::OpenFlags::empty(),
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
        match self.input.insert_capability(&protocol.name, launch.into_router().into()) {
            Ok(()) => (),
            Err(e) => warn!("failed to add {} to root component input: {e:?}", protocol.name),
        }
    }

    fn build(self) -> ComponentInput {
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

    pub realm_query: Option<Arc<RealmQuery>>,
    pub lifecycle_controller: Option<Arc<LifecycleController>>,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub event_registry: Arc<EventRegistry>,
    pub event_source_factory: Arc<EventSourceFactory>,
    pub factory_capability_host: Arc<FactoryCapabilityHost>,
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
    _component_escrow_duration_status: Arc<diagnostics::escrow::DurationStats>,
    pub debug: bool,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub num_threads: usize,
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub realm_builder_resolver: Option<Arc<RealmBuilderResolver>>,
    pub root_component_input: ComponentInput,
    capability_passthrough: bool,
    _service_fs_task: Option<fasync::Task<()>>,
}

impl BuiltinEnvironment {
    async fn new(
        model: Arc<Model>,
        runtime_config: Arc<RuntimeConfig>,
        system_resource_handle: Option<Resource>,
        builtin_runners: Vec<BuiltinRunner>,
        boot_resolver: Option<FuchsiaBootResolverBuiltinCapability>,
        realm_builder_resolver: Option<Arc<RealmBuilderResolver>>,
        utc_clock: Option<Arc<Clock>>,
        inspector: Inspector,
        crash_records: CrashRecords,
        capability_passthrough: bool,
    ) -> Result<BuiltinEnvironment, Error> {
        let debug = runtime_config.debug;

        let num_threads = runtime_config.num_threads.clone();

        let event_logger = if runtime_config.log_all_events {
            let event_logger = Arc::new(EventLogger::new());
            model.root().hooks.install(event_logger.hooks()).await;
            Some(event_logger)
        } else {
            None
        };

        let mut root_input_builder =
            RootComponentInputBuilder::new(model.top_instance().clone(), &runtime_config);

        for namespace_capability in model.top_instance().namespace_capabilities() {
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
                    .parse()?,
            ),
            None => None,
        };

        // Set up fuchsia.boot.SvcStashProvider service.
        let svc_stash_provider = take_startup_handle(HandleInfo::new(HandleType::User0, 0))
            .map(zx::Channel::from)
            .map(SvcStashCapability::new);
        if let Some(svc_stash_provider) = svc_stash_provider {
            root_input_builder.add_builtin_protocol_if_enabled::<fboot::SvcStashProviderMarker>(
                move |stream| svc_stash_provider.clone().serve(stream).boxed(),
            );
        }

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
            let utc_time_maintainer = Arc::new(UtcTimeMaintainer::new(clock));
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

        // Set up the FramebufferResource service.
        let framebuffer_resource = system_resource_handle
            .as_ref()
            .and_then(|handle| {
                handle
                    .create_child(
                        zx::ResourceKind::SYSTEM,
                        None,
                        zx::sys::ZX_RSRC_SYSTEM_FRAMEBUFFER_BASE,
                        1,
                        b"framebuffer",
                    )
                    .ok()
            })
            .map(FramebufferResource::new)
            .and_then(Result::ok);
        if let Some(framebuffer_resource) = framebuffer_resource {
            root_input_builder
                .add_builtin_protocol_if_enabled::<fkernel::FramebufferResourceMarker>(
                    move |stream| framebuffer_resource.clone().serve(stream).boxed(),
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
        let weak_model = Arc::downgrade(&model);
        root_input_builder.add_builtin_protocol_if_enabled::<fsys::SystemControllerMarker>(
            move |stream| {
                SystemController::new(weak_model.clone(), SHUTDOWN_TIMEOUT).serve(stream).boxed()
            },
        );

        // Set up the Inspect sink provider.
        let inspect_sink_provider = Arc::new(InspectSinkProvider::new(inspector));

        // Set up the event registry.
        let event_registry = {
            let mut event_registry = EventRegistry::new(Arc::downgrade(&model));
            event_registry.register_synthesis_provider(
                EventType::CapabilityRequested,
                inspect_sink_provider.clone(),
            );
            Arc::new(event_registry)
        };
        model.root().hooks.install(event_registry.hooks()).await;

        let event_stream_provider =
            Arc::new(EventStreamProvider::new(Arc::downgrade(&event_registry)));
        model.root().hooks.install(event_stream_provider.hooks()).await;

        let event_source_factory = EventSourceFactory::new(
            Arc::downgrade(model.top_instance()),
            Arc::downgrade(&event_registry),
            Arc::downgrade(&event_stream_provider),
        );

        let factory_capability_host = Arc::new(FactoryCapabilityHost::new());

        let mut builtin_capabilities: Vec<Box<dyn BuiltinCapability>> =
            vec![Box::new(EventSourceFactoryCapability::new(event_source_factory.clone()))];
        let mut framework_capabilities: Vec<Box<dyn FrameworkCapability>> = vec![
            Box::new(RealmFrameworkCapability::new(Arc::downgrade(&model), runtime_config.clone())),
            Box::new(IntrospectorFrameworkCapability {
                instance_registry: model.context().instance_registry().clone(),
            }),
            Box::new(BinderFrameworkCapability::new()),
            Box::new(FactoryFrameworkCapability::new(factory_capability_host.clone())),
            Box::new(NamespaceFrameworkCapability::new()),
            Box::new(PkgDirectoryFrameworkCapability::new()),
            Box::new(EventSourceFactoryCapability::new(event_source_factory.clone())),
        ];
        let derived_capabilities: Vec<Box<dyn DerivedCapability>> =
            vec![Box::new(StorageAdminDerivedCapability::new(Arc::downgrade(&model)))];

        // Set up the builtin runners.
        for runner in builtin_runners {
            builtin_capabilities.push(Box::new(runner));
        }

        // Set up the boot resolver so it is routable from "above root".
        if let Some(boot_resolver) = boot_resolver {
            let b = boot_resolver.host().clone();
            root_input_builder.add_builtin_protocol_if_enabled::<fresolution::ResolverMarker>(
                move |stream| {
                    let b = b.clone();
                    async move { b.serve(stream).await.map_err(|e| format_err!("{:?}", e)) }.boxed()
                },
            );
            builtin_capabilities.push(Box::new(boot_resolver));
        }

        // Set up the root realm stop notifier.
        let stop_notifier = Arc::new(RootStopNotifier::new());
        model.root().hooks.install(stop_notifier.hooks()).await;

        let realm_query = if runtime_config.enable_introspection {
            let host = RealmQuery::new(Arc::downgrade(&model));
            framework_capabilities.push(Box::new(RealmQueryFrameworkCapability::new(host.clone())));
            Some(host)
        } else {
            None
        };

        let lifecycle_controller = if runtime_config.enable_introspection {
            let host = LifecycleController::new(Arc::downgrade(&model));
            framework_capabilities
                .push(Box::new(LifecycleControllerFrameworkCapability::new(host.clone())));
            Some(host)
        } else {
            None
        };

        if runtime_config.enable_introspection {
            framework_capabilities
                .push(Box::new(RouteValidatorFrameworkCapability::new(Arc::downgrade(&model))));
        }

        model
            .context()
            .init_internal_capabilities(
                builtin_capabilities,
                framework_capabilities,
                derived_capabilities,
            )
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

        let component_escrow_duration_status = Arc::new(diagnostics::escrow::DurationStats::new(
            inspector.root().create_child("escrow"),
        ));
        model.root().hooks.install(component_escrow_duration_status.hooks()).await;

        // Serve stats about inspect in a lazy node.
        inspector.record_lazy_stats();

        let root_component_input = root_input_builder.build();

        Ok(BuiltinEnvironment {
            model,
            realm_query,
            lifecycle_controller,
            event_registry,
            event_source_factory,
            factory_capability_host,
            stop_notifier,
            inspect_sink_provider,
            event_stream_provider,
            event_logger,
            component_tree_stats,
            _component_lifecycle_time_stats: component_lifecycle_time_stats,
            _component_escrow_duration_status: component_escrow_duration_status,
            debug,
            num_threads,
            realm_builder_resolver,
            root_component_input,
            capability_passthrough,
            _service_fs_task: None,
        })
    }

    /// Returns a ServiceFs that contains protocols served by component manager.
    async fn create_service_fs<'a>(&self) -> Result<ServiceFs<ServiceObj<'a, ()>>, Error> {
        // Create the ServiceFs
        let mut service_fs = ServiceFs::new();

        let scope = self.model.top_instance().task_group().clone();

        // Install the root fuchsia.sys2.LifecycleController
        if let Some(lifecycle_controller) = &self.lifecycle_controller {
            let lifecycle_controller = lifecycle_controller.clone();
            let scope = scope.clone();
            service_fs.dir("svc").add_fidl_service(move |stream| {
                let lifecycle_controller = lifecycle_controller.clone();
                // Spawn a short-lived task that adds the lifecycle controller serve to
                // component manager's task scope.
                scope.spawn(async move {
                    lifecycle_controller.serve(Moniker::root(), stream).await;
                });
            });
        }

        // Install the root fuchsia.sys2.RealmQuery
        if let Some(realm_query) = &self.realm_query {
            let realm_query = realm_query.clone();
            let scope = scope.clone();
            service_fs.dir("svc").add_fidl_service(move |stream| {
                let realm_query = realm_query.clone();
                // Spawn a short-lived task that adds the realm query serve to
                // component manager's task scope.
                scope.spawn(async move {
                    realm_query.serve(Moniker::root(), stream).await;
                });
            });
        }

        // Install the `fuchsia.component.sandbox.Factory` protocol.
        let factory_capability_host = self.factory_capability_host.clone();
        {
            let scope = scope.clone();
            service_fs.dir("svc").add_fidl_service(move |stream| {
                let factory_capability_host = factory_capability_host.clone();
                scope.spawn(async move {
                    if let Err(err) = factory_capability_host.serve(stream).await {
                        warn!(?err, "Failed to serve fuchsia.component.sandbox.Factory");
                    }
                });
            });
        }

        // If capability passthrough is enabled, add a remote directory to proxy
        // capabilities exposed by the root component.
        if self.capability_passthrough {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;
            service_fs.add_remote("root-exposed", proxy);
            let root = self.model.top_instance().root().await;
            let root = WeakComponentInstance::new(&root);
            scope.clone().spawn(async move {
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
                                        source_name: EventType::Discovered.into(),
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
    pub async fn bind_service_fs_to_out(&mut self) -> Result<(), Error> {
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

    #[cfg(test)]
    /// Adds a protocol to the root dict, replacing prior entries. This must be called before
    /// the model is started.
    pub async fn add_protocol_to_root_dict<P>(
        &mut self,
        name: Name,
        task_to_launch: impl Fn(P::RequestStream) -> BoxFuture<'static, Result<(), anyhow::Error>>
            + Sync
            + Send
            + 'static,
    ) where
        P: ProtocolMarker,
    {
        let capability_source = CapabilitySource::Builtin {
            capability: InternalCapability::Protocol(name.clone()),
            top_instance: Arc::downgrade(self.model.top_instance()),
        };

        let launch = LaunchTaskOnReceive::new(
            self.model.top_instance().task_group().as_weak(),
            name.clone(),
            Some((self.model.root().context.policy().clone(), capability_source)),
            Arc::new(move |server_end, _| {
                task_to_launch(crate::sandbox_util::take_handle_as_stream::<P>(server_end)).boxed()
            }),
        );
        self.root_component_input.insert_capability(&name, launch.into_router().into()).unwrap();
    }

    #[cfg(test)]
    /// Causes the root component to be discovered, which provides the root component with the
    /// dict from the builtin environment. This is called in some tests because the tests create
    /// a new model but do not call `Model::start`.
    pub async fn discover_root_component(&self) {
        self.model.discover_root_component(self.root_component_input.clone()).await;
    }

    pub async fn wait_for_root_stop(&self) {
        self.stop_notifier.wait_for_root_stop().await;
    }

    pub async fn run_root(&mut self) -> Result<(), Error> {
        self.bind_service_fs_to_out().await?;
        self.model.start(self.root_component_input.clone()).await;
        component::health().set_ok();
        self.wait_for_root_stop().await;

        // Stop serving the out directory, so that more connections to debug capabilities
        // cannot be made.
        drop(self._service_fs_task.take());
        Ok(())
    }

    #[cfg(test)]
    pub fn inspector(&self) -> &Inspector {
        self.inspect_sink_provider.inspector()
    }
}

fn register_builtin_resolver(resolvers: &mut ResolverRegistry) {
    resolvers.register(BUILTIN_SCHEME.to_string(), Box::new(BuiltinResolver {}));
}

// Creates a FuchsiaBootResolver if the /boot directory is installed in component_manager's
// namespace, and registers it with the ResolverRegistry. The resolver is returned to so that
// it can be installed as a Builtin capability.
async fn register_boot_resolver(
    resolvers: &mut ResolverRegistry,
    runtime_config: &RuntimeConfig,
) -> Result<Option<FuchsiaBootResolverBuiltinCapability>, Error> {
    let path = match &runtime_config.builtin_boot_resolver {
        BuiltinBootResolver::Boot => "/boot",
        BuiltinBootResolver::None => return Ok(None),
    };
    let boot_resolver = FuchsiaBootResolverBuiltinCapability::new(path)
        .await
        .context("Failed to create boot resolver")?;
    match boot_resolver {
        None => {
            info!(%path, "fuchsia-boot resolver unavailable, not in namespace");
            Ok(None)
        }
        Some(boot_resolver) => {
            resolvers.register(BOOT_SCHEME.to_string(), box_arc_resolver(boot_resolver.host()));
            Ok(Some(boot_resolver))
        }
    }
}

fn register_realm_builder_resolver(
    resolvers: &mut ResolverRegistry,
) -> Result<Arc<RealmBuilderResolver>, Error> {
    let realm_builder_resolver =
        RealmBuilderResolver::new().context("Failed to create realm builder resolver")?;
    let resolver = Arc::new(realm_builder_resolver);
    resolvers.register(REALM_BUILDER_SCHEME.to_string(), box_arc_resolver(&resolver));
    Ok(resolver)
}
