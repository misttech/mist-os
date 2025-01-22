// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    expose_root, get_serial_number, parse_features, parse_numbered_handles, run_container_features,
    serve_component_runner, serve_container_controller, serve_graphical_presenter, Features,
    MountAction,
};
use anyhow::{anyhow, bail, Context, Error};
use bstr::BString;
use fasync::OnSignals;
use fidl::endpoints::{ControlHandle, RequestStream, ServerEnd};
use fidl_fuchsia_component_runner::{TaskProviderRequest, TaskProviderRequestStream};
use fidl_fuchsia_feedback::CrashReporterMarker;
use fidl_fuchsia_scheduler::RoleManagerMarker;
use fuchsia_async::DurationExt;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_sync};
use fuchsia_component::server::ServiceFs;
use futures::channel::oneshot;
use futures::{FutureExt, StreamExt, TryStreamExt};
use runner::{get_program_string, get_program_strvec};
use starnix_core::container_namespace::ContainerNamespace;
use starnix_core::execution::execute_task_with_prerun_result;
use starnix_core::fs::fuchsia::create_remotefs_filesystem;
use starnix_core::fs::tmpfs::TmpFs;
use starnix_core::security;
use starnix_core::task::{set_thread_role, CurrentTask, ExitStatus, Kernel, Task};
use starnix_core::time::utc::update_utc_clock;
use starnix_core::vfs::{
    FileSystemOptions, FsContext, LookupContext, Namespace, StaticDirectoryBuilder, WhatToMount,
};
use starnix_logging::{
    log_error, log_info, log_warn, trace_duration, CATEGORY_STARNIX, NAME_CREATE_CONTAINER,
};
use starnix_modules::{init_common_devices, register_common_file_systems};
use starnix_modules_layeredfs::LayeredFs;
use starnix_modules_magma::get_magma_params;
use starnix_modules_overlayfs::OverlayStack;
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::errors::{SourceContext, ENOENT};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::resource_limits::Resource;
use starnix_uapi::{errno, pid_t, rlimit};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::ops::DerefMut;
use std::sync::Arc;
use zx::{
    AsHandleRef, Signals, Task as _, {self as zx},
};
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_runner as frunner,
    fidl_fuchsia_element as felement, fidl_fuchsia_io as fio,
    fidl_fuchsia_memory_attribution as fattribution,
    fidl_fuchsia_starnix_container as fstarcontainer, fuchsia_async as fasync,
    fuchsia_inspect as inspect, fuchsia_runtime as fruntime,
};

use std::sync::Weak;

use crate::serve_memory_attribution_provider_container;
use attribution_server::{AttributionServer, AttributionServerHandle};
use fidl::HandleBased;

/// Manages the memory attribution protocol for a Starnix container.
struct ContainerMemoryAttributionManager {
    /// Holds state for the hanging-get attribution protocol.
    memory_attribution_server: AttributionServerHandle,
}

impl ContainerMemoryAttributionManager {
    /// Creates a new [ContainerMemoryAttributionManager] from a Starnix kernel and the moniker
    /// token of the container component.
    pub fn new(kernel: Weak<Kernel>, component_instance: zx::Event) -> Self {
        let memory_attribution_server = AttributionServer::new(Box::new(move || {
            let kernel_ref = match kernel.upgrade() {
                None => return vec![],
                Some(k) => k,
            };
            attribution_info_for_kernel(kernel_ref.as_ref(), &component_instance)
        }));

        ContainerMemoryAttributionManager { memory_attribution_server }
    }

    /// Creates a new observer for the attribution information from this container.
    pub fn new_observer(
        &self,
        control_handle: fattribution::ProviderControlHandle,
    ) -> attribution_server::Observer {
        self.memory_attribution_server.new_observer(control_handle)
    }
}

/// Generates the attribution information for the Starnix kernel ELF component. The attribution
/// information for the container is handled by the container component, not the kernel
/// component itself, even if both are hosted within the same kernel process.
fn attribution_info_for_kernel(
    kernel: &Kernel,
    component_instance: &zx::Event,
) -> Vec<fattribution::AttributionUpdate> {
    // Start the server to handle the memory attribution requests for the container, and provide
    // a handle to get detailed attribution. We start a new task as each incoming connection is
    // independent.
    let (client_end, server_end) =
        fidl::endpoints::create_request_stream::<fattribution::ProviderMarker>();
    fuchsia_async::Task::spawn(serve_memory_attribution_provider_container(server_end, kernel))
        .detach();

    let new_principal = fattribution::NewPrincipal {
        identifier: Some(component_instance.get_koid().unwrap().raw_koid()),
        description: Some(fattribution::Description::Component(
            component_instance.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
        )),
        principal_type: Some(fattribution::PrincipalType::Runnable),
        detailed_attribution: Some(client_end),
        ..Default::default()
    };
    let attribution = fattribution::UpdatedPrincipal {
        identifier: Some(component_instance.get_koid().unwrap().raw_koid()),
        resources: Some(fattribution::Resources::Data(fattribution::Data {
            resources: vec![fattribution::Resource::KernelObject(
                fuchsia_runtime::job_default().get_koid().unwrap().raw_koid(),
            )],
        })),
        ..Default::default()
    };
    vec![
        fattribution::AttributionUpdate::Add(new_principal),
        fattribution::AttributionUpdate::Update(attribution),
    ]
}

pub struct Config {
    /// The features enabled for this container.
    pub features: Vec<String>,

    /// The command line for the initial process for this container.
    init: Vec<String>,

    /// The command line for the kernel.
    kernel_cmdline: String,

    /// The specifications for the file system mounts for this container.
    mounts: Vec<String>,

    /// The resource limits to apply to this container.
    rlimits: Vec<String>,

    /// The name of this container.
    name: String,

    /// The path that the container will wait until exists before considering itself to have started.
    startup_file_path: String,

    /// The remote block devices to use for the container.
    remote_block_devices: Vec<String>,

    /// The outgoing directory of the container, used to serve protocols on behalf of the container.
    /// For example, the starnix_kernel serves a component runner in the containers' outgoing
    /// directory.
    outgoing_dir: Option<zx::Channel>,

    /// Mapping of top-level namespace entries to an associated channel.
    /// For example, "/svc" to the respective channel.
    container_namespace: ContainerNamespace,

    /// The runtime directory of the container, used to provide CF introspection.
    runtime_dir: Option<ServerEnd<fio::DirectoryMarker>>,

    /// The default seclabel that is applied to components that are instantiated in this container.
    ///
    /// Components can override this by setting the `seclabel` field in their program block.
    pub default_seclabel: Option<String>,

    /// The default fsseclabel that is applied to components that are instantiated in this container.
    ///
    /// Components can override this by setting the `fsseclabel` field in their program block.
    pub default_fsseclabel: Option<String>,

    /// The default uid that is applied to components that are instantiated in this container.
    ///
    /// Components can override this by setting the `uid` field in their program block.
    pub default_uid: u32,

    /// Component moniker token for the container component. This token is used in various protocols
    /// to uniquely identify a component.
    component_instance: Option<zx::Event>,
}

fn get_config_from_component_start_info(mut start_info: frunner::ComponentStartInfo) -> Config {
    let get_strvec = |key| {
        get_program_strvec(&start_info, key)
            .unwrap_or_default()
            .map(|value| value.to_owned())
            .unwrap_or_default()
    };

    let get_string = |key| get_program_string(&start_info, key).unwrap_or_default().to_owned();

    let default_uid = get_program_string(&start_info, "default_uid")
        .map(|s| s.parse().expect("container default uid"))
        .unwrap_or(42);
    let default_seclabel = get_program_string(&start_info, "default_seclabel").map(|s| s.into());
    let default_fsseclabel =
        get_program_string(&start_info, "default_fsseclabel").map(|s| s.into());
    let features = get_strvec("features");
    let init = get_strvec("init");
    let kernel_cmdline = get_string("kernel_cmdline");
    let mounts = get_strvec("mounts");
    let name = get_string("name");
    let remote_block_devices = get_strvec("remote_block_devices");
    let rlimits = get_strvec("rlimits");
    let startup_file_path = get_string("startup_file_path");
    let ns = start_info.ns.take().expect("Unable to access container namespace!");
    let container_namespace = ContainerNamespace::from(ns);

    let outgoing_dir = start_info.outgoing_dir.take().map(|dir| dir.into_channel());
    let component_instance = start_info.component_instance;

    Config {
        default_uid,
        default_seclabel,
        default_fsseclabel,
        features,
        init,
        kernel_cmdline,
        mounts,
        rlimits,
        name,
        startup_file_path,
        remote_block_devices,
        outgoing_dir,
        container_namespace,
        component_instance,
        runtime_dir: start_info.runtime_dir,
    }
}

// Creates a CString from a String. Calling this with an invalid CString will panic.
fn to_cstr(str: &str) -> CString {
    CString::new(str.to_string()).unwrap()
}

#[must_use = "The container must run serve on this config"]
pub struct ContainerServiceConfig {
    config: Config,
    request_stream: frunner::ComponentControllerRequestStream,
    receiver: oneshot::Receiver<Result<ExitStatus, Error>>,
}

pub struct Container {
    /// The `Kernel` object that is associated with the container.
    pub kernel: Arc<Kernel>,

    memory_attribution_manager: ContainerMemoryAttributionManager,

    /// Inspect node holding information about the state of the container.
    _node: inspect::Node,

    /// Until negative trait bound are implemented, using `*mut u8` to prevent transferring
    /// Container across threads.
    _thread_bound: std::marker::PhantomData<*mut u8>,
}

impl Container {
    pub fn system_task(&self) -> &CurrentTask {
        self.kernel.kthreads.system_task()
    }

    async fn serve_outgoing_directory(
        &self,
        outgoing_dir: Option<zx::Channel>,
    ) -> Result<(), Error> {
        if let Some(outgoing_dir) = outgoing_dir {
            // Add `ComponentRunner` to the exposed services of the container, and then serve the
            // outgoing directory.
            let mut fs = ServiceFs::new_local();
            fs.dir("svc")
                .add_fidl_service(ExposedServices::ComponentRunner)
                .add_fidl_service(ExposedServices::ContainerController)
                .add_fidl_service(ExposedServices::GraphicalPresenter);

            // Expose the root of the container's filesystem.
            let (fs_root, fs_root_server_end) = fidl::endpoints::create_proxy();
            fs.add_remote("fs_root", fs_root);
            expose_root(
                self.kernel.kthreads.unlocked_for_async().deref_mut(),
                self.system_task(),
                fs_root_server_end,
            )?;

            fs.serve_connection(outgoing_dir.into()).map_err(|_| errno!(EINVAL))?;

            fs.for_each_concurrent(None, |request_stream| async {
                match request_stream {
                    ExposedServices::ComponentRunner(request_stream) => {
                        match serve_component_runner(request_stream, self.system_task()).await {
                            Ok(_) => {}
                            Err(e) => {
                                log_error!("Error serving component runner: {:?}", e);
                            }
                        }
                    }
                    ExposedServices::ContainerController(request_stream) => {
                        serve_container_controller(request_stream, self.system_task())
                            .await
                            .expect("failed to start container.")
                    }
                    ExposedServices::GraphicalPresenter(request_stream) => {
                        serve_graphical_presenter(request_stream, &self.kernel)
                            .await
                            .expect("failed to start GraphicalPresenter.")
                    }
                }
            })
            .await
        }
        Ok(())
    }

    pub async fn serve(&self, service_config: ContainerServiceConfig) -> Result<(), Error> {
        let (r, _) = futures::join!(
            self.serve_outgoing_directory(service_config.config.outgoing_dir),
            server_component_controller(service_config.request_stream, service_config.receiver)
        );
        r
    }

    pub fn new_memory_attribution_observer(
        &self,
        control_handle: fattribution::ProviderControlHandle,
    ) -> attribution_server::Observer {
        self.memory_attribution_manager.new_observer(control_handle)
    }
}

/// The services that are exposed in the container component's outgoing directory.
enum ExposedServices {
    ComponentRunner(frunner::ComponentRunnerRequestStream),
    ContainerController(fstarcontainer::ControllerRequestStream),
    GraphicalPresenter(felement::GraphicalPresenterRequestStream),
}

type TaskResult = Result<ExitStatus, Error>;

async fn server_component_controller(
    request_stream: frunner::ComponentControllerRequestStream,
    task_complete: oneshot::Receiver<TaskResult>,
) {
    let request_stream_control = request_stream.control_handle();

    enum Event<T, U> {
        Controller(T),
        Completion(U),
    }

    let mut stream = futures::stream::select(
        request_stream.map(Event::Controller),
        task_complete.into_stream().map(Event::Completion),
    );

    if let Some(event) = stream.next().await {
        match event {
            Event::Controller(_) => {
                // If we get a `Stop` request, we would ideally like to ask userspace to shut
                // down gracefully.
            }
            Event::Completion(result) => {
                match result {
                    Ok(Ok(ExitStatus::Exit(0))) => {
                        request_stream_control.shutdown_with_epitaph(zx::Status::OK)
                    }
                    _ => request_stream_control.shutdown_with_epitaph(zx::Status::from_raw(
                        fcomponent::Error::InstanceDied.into_primitive() as i32,
                    )),
                };
            }
        }
    }
    // Kill the starnix_kernel job, as the kernel is expected to reboot when init exits.
    fruntime::job_default().kill().expect("Failed to kill job");
}

pub async fn create_component_from_stream(
    mut request_stream: frunner::ComponentRunnerRequestStream,
    structured_config: &starnix_kernel_structured_config::Config,
) -> Result<(Container, ContainerServiceConfig), Error> {
    if let Some(event) = request_stream.try_next().await? {
        match event {
            frunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                let request_stream = controller.into_stream();
                let mut config = get_config_from_component_start_info(start_info);
                let (sender, receiver) = oneshot::channel::<TaskResult>();
                let container = create_container(&mut config, sender, structured_config)
                    .await
                    .with_source_context(|| format!("creating container \"{}\"", &config.name))?;
                let service_config = ContainerServiceConfig { config, request_stream, receiver };

                container.kernel.kthreads.spawn_future({
                    let vvar = container.kernel.vdso.vvar_writeable.clone();
                    let utc_clock =
                        fruntime::duplicate_utc_clock_handle(zx::Rights::SAME_RIGHTS).unwrap();
                    async move {
                        loop {
                            let waitable =
                                OnSignals::new(utc_clock.as_handle_ref(), Signals::CLOCK_UPDATED);
                            update_utc_clock(&vvar);
                            waitable.await.expect("async_wait should always succeed");
                            log_info!("Received a UTC update");
                        }
                    }
                });
                return Ok((container, service_config));
            }
            frunner::ComponentRunnerRequest::_UnknownMethod { ordinal, .. } => {
                log_warn!("Unknown ComponentRunner request: {ordinal}");
            }
        }
    }
    bail!("did not receive Start request");
}

async fn create_container(
    config: &mut Config,
    task_complete: oneshot::Sender<TaskResult>,
    structured_config: &starnix_kernel_structured_config::Config,
) -> Result<Container, Error> {
    trace_duration!(CATEGORY_STARNIX, NAME_CREATE_CONTAINER);
    const DEFAULT_INIT: &str = "/container/init";

    let pkg_channel = config.container_namespace.get_namespace_channel("/pkg").unwrap();
    let pkg_dir_proxy = fio::DirectorySynchronousProxy::new(pkg_channel);

    let features = parse_features(&config, structured_config)?;
    let mut kernel_cmdline = BString::from(config.kernel_cmdline.as_bytes());
    if features.android_serialno {
        match get_serial_number().await {
            Ok(serial) => {
                kernel_cmdline.extend(b" androidboot.serialno=");
                kernel_cmdline.extend(&*serial);
            }
            Err(err) => log_warn!("could not get serial number: {err:?}"),
        }
    }
    if let Some(supported_vendors) = &features.magma_supported_vendors {
        kernel_cmdline.extend(b" ");
        let params = get_magma_params(supported_vendors);
        kernel_cmdline.extend(&*params);
    }

    // Collect a vector of functions to be invoked while constructing /proc/device-tree
    let mut procfs_device_tree_setup: Vec<fn(&mut StaticDirectoryBuilder<'_>, &CurrentTask)> =
        Vec::new();
    if features.nanohub {
        procfs_device_tree_setup.push(starnix_modules_nanohub::nanohub_procfs_builder);
    }

    // Check whether we actually have access to a role manager by trying to set our own
    // thread's role.
    let role_manager = connect_to_protocol_sync::<RoleManagerMarker>().unwrap();
    let role_manager = if let Err(e) =
        set_thread_role(&role_manager, &*fuchsia_runtime::thread_self(), Default::default())
    {
        log_warn!("Setting thread role failed ({e:?}), will not set thread priority.");
        None
    } else {
        log_info!("Thread role set successfully.");
        Some(role_manager)
    };

    let crash_reporter = connect_to_protocol::<CrashReporterMarker>().unwrap();

    let node = inspect::component::inspector().root().create_child("container");
    let kernel_node = node.create_child("kernel");
    kernel_node.record_int("created_at", zx::MonotonicInstant::get().into_nanos());
    features.record_inspect(&kernel_node);

    let selinux_exceptions_config = if let Some(ref file_path) = features.selinux.exceptions_path {
        let (file, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();

        let flags = fio::Flags::PERM_READ | fio::Flags::PROTOCOL_FILE;

        pkg_dir_proxy
            .open3(&file_path, flags, &fio::Options::default(), server_end.into_channel())
            .expect("failed to open security exception file");

        let contents =
            fuchsia_fs::file::read(&file).await.expect("reading security exception file");
        String::from_utf8(contents).expect("parsing security exception file")
    } else {
        String::new()
    };
    let security_state =
        security::kernel_init_security(features.selinux.enabled, selinux_exceptions_config);

    let kernel = Kernel::new(
        kernel_cmdline,
        features.kernel.clone(),
        config.container_namespace.try_clone()?,
        role_manager,
        Some(crash_reporter),
        kernel_node,
        features.aspect_ratio.as_ref(),
        security_state,
        procfs_device_tree_setup,
    )
    .with_source_context(|| format!("creating Kernel: {}", &config.name))?;
    let fs_context = create_fs_context(
        kernel.kthreads.unlocked_for_async().deref_mut(),
        &kernel,
        &features,
        config,
        &pkg_dir_proxy,
    )
    .source_context("creating FsContext")?;
    let init_pid = kernel.pids.write().allocate_pid();
    // Lots of software assumes that the pid for the init process is 1.
    debug_assert_eq!(init_pid, 1);

    let system_task = CurrentTask::create_system_task(
        kernel.kthreads.unlocked_for_async().deref_mut(),
        &kernel,
        Arc::clone(&fs_context),
    )
    .source_context("create system task")?;
    // The system task gives pid 2. This value is less critical than giving
    // pid 1 to init, but this value matches what is supposed to happen.
    debug_assert_eq!(system_task.id, 2);

    kernel.kthreads.init(system_task).source_context("initializing kthreads")?;
    let system_task = kernel.kthreads.system_task();

    kernel.syslog.init(&system_task).source_context("initializing syslog")?;

    kernel
        .hrtimer_manager
        .init(system_task, None)
        .source_context("initializing HrTimer manager")?;

    if let Err(e) = kernel.suspend_resume_manager.init(&system_task) {
        log_warn!("Suspend/Resume manager initialization failed: ({e:?})");
    }

    // Register common devices and add them in sysfs and devtmpfs.
    init_common_devices(kernel.kthreads.unlocked_for_async().deref_mut(), &system_task);
    register_common_file_systems(kernel.kthreads.unlocked_for_async().deref_mut(), &kernel);

    mount_filesystems(
        kernel.kthreads.unlocked_for_async().deref_mut(),
        &system_task,
        config,
        &pkg_dir_proxy,
    )
    .source_context("mounting filesystems")?;

    // Run all common features that were specified in the .cml.
    {
        run_container_features(
            kernel.kthreads.unlocked_for_async().deref_mut(),
            &system_task,
            &features,
        )?;
    }

    if features.android_fdr {
        init_remote_block_devices(
            kernel.kthreads.unlocked_for_async().deref_mut(),
            &system_task,
            config,
        )
        .source_context("initalizing remote block devices")?;
    }

    // If there is an init binary path, run it, optionally waiting for the
    // startup_file_path to be created. The task struct is still used
    // to initialize the system up until this point, regardless of whether
    // or not there is an actual init to be run.
    let argv =
        if config.init.is_empty() { vec![DEFAULT_INIT.to_string()] } else { config.init.clone() }
            .iter()
            .map(|s| to_cstr(s))
            .collect::<Vec<_>>();

    let executable = system_task
        .open_file(
            kernel.kthreads.unlocked_for_async().deref_mut(),
            argv[0].as_bytes().into(),
            OpenFlags::RDONLY,
        )
        .with_source_context(|| format!("opening init: {:?}", &argv[0]))?;

    let initial_name = if config.init.is_empty() {
        CString::default()
    } else {
        CString::new(config.init[0].clone())?
    };

    let rlimits = parse_rlimits(&config.rlimits)?;
    let init_task = CurrentTask::create_init_process(
        kernel.kthreads.unlocked_for_async().deref_mut(),
        &kernel,
        init_pid,
        initial_name,
        Arc::clone(&fs_context),
        &rlimits,
    )
    .with_source_context(|| format!("creating init task: {:?}", &config.init))?;

    execute_task_with_prerun_result(
        kernel.kthreads.unlocked_for_async().deref_mut(),
        init_task,
        move |locked, init_task| {
            parse_numbered_handles(init_task, None, &init_task.files).expect("");
            init_task.exec(locked, executable, argv[0].clone(), argv.clone(), vec![])
        },
        move |result| {
            log_info!("Finished running init process: {:?}", result);
            let _ = task_complete.send(result);
        },
        None,
    )?;

    if !config.startup_file_path.is_empty() {
        wait_for_init_file(&config.startup_file_path, &system_task, init_pid).await?;
    };

    let memory_attribution_manager = ContainerMemoryAttributionManager::new(
        Arc::downgrade(&kernel),
        config.component_instance.take().ok_or_else(|| Error::msg("No component instance"))?,
    );

    // Serve the runtime directory.
    if let Some(runtime_dir) = config.runtime_dir.take() {
        kernel.kthreads.spawn_future(serve_runtime_dir(runtime_dir));
    }

    Ok(Container {
        kernel,
        memory_attribution_manager,
        _node: node,
        _thread_bound: Default::default(),
    })
}

fn create_fs_context(
    locked: &mut Locked<'_, Unlocked>,
    kernel: &Arc<Kernel>,
    features: &Features,
    config: &Config,
    pkg_dir_proxy: &fio::DirectorySynchronousProxy,
) -> Result<Arc<FsContext>, Error> {
    // The mounts are applied in the order listed. Mounting will fail if the designated mount
    // point doesn't exist in a previous mount. The root must be first so other mounts can be
    // applied on top of it.
    let mut mounts_iter = config.mounts.iter();
    let mut root = MountAction::new_for_root(
        locked,
        kernel,
        pkg_dir_proxy,
        mounts_iter.next().ok_or_else(|| anyhow!("Mounts list is empty"))?,
    )?;
    if root.path != "/" {
        anyhow::bail!("First mount in mounts list is not the root");
    }

    // Create a layered fs to handle /container and /container/component
    let mut mappings = vec![];
    if features.container {
        // /container will mount the container pkg
        // /container/component will be a tmpfs where component using the starnix kernel will have their
        // package mounted.
        let rights = fio::PERM_READABLE | fio::PERM_EXECUTABLE;
        let container_fs = LayeredFs::new_fs(
            kernel,
            create_remotefs_filesystem(
                kernel,
                pkg_dir_proxy,
                FileSystemOptions { source: "data".into(), ..Default::default() },
                rights,
            )?,
            BTreeMap::from([("component".into(), TmpFs::new_fs(kernel))]),
        );
        mappings.push(("container".into(), container_fs));
    }
    if features.custom_artifacts {
        mappings.push(("custom_artifacts".into(), TmpFs::new_fs(kernel)));
    }
    if features.test_data {
        mappings.push(("test_data".into(), TmpFs::new_fs(kernel)));
    }

    if !mappings.is_empty() {
        // If this container has enabled any features that mount directories that might not exist
        // in the root file system, we add a LayeredFs to hold these mappings.
        root.fs = LayeredFs::new_fs(kernel, root.fs, mappings.into_iter().collect());
    }
    if features.rootfs_rw {
        root.fs = OverlayStack::wrap_fs_in_writable_layer(kernel, root.fs)?;
    }
    Ok(FsContext::new(Namespace::new_with_flags(root.fs, root.flags)))
}

pub fn set_rlimits(task: &Task, rlimits: &[String]) -> Result<(), Error> {
    let set_rlimit = |resource, value| {
        task.thread_group.limits.lock().set(resource, rlimit { rlim_cur: value, rlim_max: value });
    };

    for rlimit in rlimits.iter() {
        let (key, value) =
            rlimit.split_once('=').ok_or_else(|| anyhow!("Invalid rlimit: {rlimit}"))?;
        let value = value.parse::<u64>()?;
        match key {
            "RLIMIT_NOFILE" => set_rlimit(Resource::NOFILE, value),
            _ => {
                bail!("Unknown rlimit: {key}");
            }
        }
    }
    Ok(())
}

fn parse_rlimits(rlimits: &[String]) -> Result<Vec<(Resource, u64)>, Error> {
    let mut res = Vec::new();

    for rlimit in rlimits {
        let (key, value) =
            rlimit.split_once('=').ok_or_else(|| anyhow!("Invalid rlimit: {rlimit}"))?;
        let value = value.parse::<u64>()?;
        let kv = match key {
            "RLIMIT_NOFILE" => (Resource::NOFILE, value),
            _ => bail!("Unknown rlimit: {key}"),
        };
        res.push(kv);
    }

    Ok(res)
}

fn mount_filesystems(
    locked: &mut Locked<'_, Unlocked>,
    system_task: &CurrentTask,
    config: &Config,
    pkg_dir_proxy: &fio::DirectorySynchronousProxy,
) -> Result<(), Error> {
    let mut mounts_iter = config.mounts.iter();
    // Skip the first mount, that was used to create the root filesystem.
    let _ = mounts_iter.next();
    for mount_spec in mounts_iter {
        let action = MountAction::from_spec(locked, system_task, pkg_dir_proxy, mount_spec)
            .with_source_context(|| format!("creating filesystem from spec: {}", &mount_spec))?;
        let mount_point = system_task
            .lookup_path_from_root(locked, action.path.as_ref())
            .with_source_context(|| format!("lookup path from root: {}", action.path))?;
        mount_point.mount(WhatToMount::Fs(action.fs), action.flags)?;
    }
    Ok(())
}

fn init_remote_block_devices(
    locked: &mut Locked<'_, Unlocked>,
    system_task: &CurrentTask,
    config: &Config,
) -> Result<(), Error> {
    let devices_iter = config.remote_block_devices.iter();
    for device_spec in devices_iter {
        create_remote_block_device_from_spec(locked, system_task, device_spec)
            .with_source_context(|| format!("creating remoteblk from spec: {}", &device_spec))?;
    }
    Ok(())
}

fn parse_block_size(block_size_str: &str) -> Result<u64, Error> {
    if block_size_str.is_empty() {
        return Err(anyhow!("Invalid empty block size"));
    }
    let (mut string, suffix) = block_size_str.split_at(block_size_str.len() - 1);
    let multiplier: u64 = match suffix {
        "K" => 1024,
        "M" => 1024 * 1024,
        "G" => 1024 * 1024 * 1024,
        _ => {
            string = block_size_str;
            1
        }
    };
    u64::from_str_radix(string, 10)
        .map_err(|_| anyhow!("Invalid block size {string}"))
        .and_then(|val| multiplier.checked_mul(val).ok_or_else(|| anyhow!("Block size overflow")))
}

fn create_remote_block_device_from_spec<'a>(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    spec: &'a str,
) -> Result<(), Error> {
    let mut iter = spec.splitn(2, ':');
    let device_name =
        iter.next().ok_or_else(|| anyhow!("remoteblk name is missing from {:?}", spec))?;
    let device_size =
        iter.next().ok_or_else(|| anyhow!("remoteblk size is missing from {:?}", spec))?;
    let device_size = parse_block_size(device_size)?;

    current_task.kernel().remote_block_device_registry.create_remote_block_device_if_absent(
        locked,
        current_task,
        device_name,
        device_size,
    )
}

async fn wait_for_init_file(
    startup_file_path: &str,
    current_task: &CurrentTask,
    init_pid: pid_t,
) -> Result<(), Error> {
    // TODO(https://fxbug.dev/42178400): Use inotify machinery to wait for the file.
    loop {
        fasync::Timer::new(fasync::MonotonicDuration::from_millis(100).after_now()).await;
        let root = current_task.fs().root();
        let mut context = LookupContext::default();
        match current_task.lookup_path(
            current_task.kernel().kthreads.unlocked_for_async().deref_mut(),
            &mut context,
            root,
            startup_file_path.into(),
        ) {
            Ok(_) => break,
            Err(error) if error == ENOENT => {}
            Err(error) => return Err(anyhow::Error::from(error)),
        }

        if current_task.get_task(init_pid).upgrade().is_none() {
            return Err(anyhow!("Init task terminated before startup_file_path was ready"));
        }
    }
    Ok(())
}

async fn serve_runtime_dir(runtime_dir: ServerEnd<fio::DirectoryMarker>) {
    let mut fs = fuchsia_component::server::ServiceFs::new();
    match fs.serve_connection(runtime_dir) {
        Ok(_) => {
            fs.add_fidl_service(|job_requests: TaskProviderRequestStream| {
                fuchsia_async::Task::local(async move {
                    if let Err(e) = serve_task_provider(job_requests).await {
                        log_warn!(e:?; "Error serving TaskProvider");
                    }
                })
                .detach();
            });
            fs.collect::<()>().await;
        }
        Err(e) => log_error!("Couldn't serve runtime directory: {e:?}"),
    }
}

async fn serve_task_provider(mut job_requests: TaskProviderRequestStream) -> Result<(), Error> {
    while let Some(request) = job_requests.next().await {
        match request.context("getting next TaskProvider request")? {
            TaskProviderRequest::GetJob { responder } => {
                responder
                    .send(
                        fuchsia_runtime::job_default()
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .map_err(|s| s.into_raw()),
                    )
                    .context("sending job for runtime dir")?;
            }
            unknown => bail!("Unknown TaskProvider method {unknown:?}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::wait_for_init_file;
    use fuchsia_async as fasync;
    use futures::{SinkExt, StreamExt};
    use starnix_core::testing::create_kernel_task_and_unlocked;
    use starnix_core::vfs::FdNumber;
    use starnix_uapi::file_mode::{AccessCheck, FileMode};
    use starnix_uapi::open_flags::OpenFlags;
    use starnix_uapi::signals::SIGCHLD;
    use starnix_uapi::vfs::ResolveFlags;
    use starnix_uapi::CLONE_FS;

    #[fuchsia::test]
    async fn test_init_file_already_exists() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (mut sender, mut receiver) = futures::channel::mpsc::unbounded();

        let path = "/path";
        current_task
            .open_file_at(
                &mut locked,
                FdNumber::AT_FDCWD,
                path.into(),
                OpenFlags::CREAT,
                FileMode::default(),
                ResolveFlags::empty(),
                AccessCheck::default(),
            )
            .expect("Failed to create file");

        fasync::Task::local(async move {
            wait_for_init_file(path, &current_task, current_task.get_pid())
                .await
                .expect("failed to wait for file");
            sender.send(()).await.expect("failed to send message");
        })
        .detach();

        // Wait for the file creation to have been detected.
        assert!(receiver.next().await.is_some());
    }

    #[fuchsia::test]
    async fn test_init_file_wait_required() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (mut sender, mut receiver) = futures::channel::mpsc::unbounded();

        let init_task =
            current_task.clone_task_for_test(&mut locked, CLONE_FS as u64, Some(SIGCHLD));
        let path = "/path";

        let test_init_pid = current_task.get_pid();
        fasync::Task::local(async move {
            sender.send(()).await.expect("failed to send message");
            wait_for_init_file(path, &init_task, test_init_pid)
                .await
                .expect("failed to wait for file");
            sender.send(()).await.expect("failed to send message");
        })
        .detach();

        // Wait for message that file check has started.
        assert!(receiver.next().await.is_some());

        // Create the file that is being waited on.
        current_task
            .open_file_at(
                &mut locked,
                FdNumber::AT_FDCWD,
                path.into(),
                OpenFlags::CREAT,
                FileMode::default(),
                ResolveFlags::empty(),
                AccessCheck::default(),
            )
            .expect("Failed to create file");

        // Wait for the file creation to be detected.
        assert!(receiver.next().await.is_some());
    }

    #[fuchsia::test]
    async fn test_init_exits_before_file_exists() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (mut sender, mut receiver) = futures::channel::mpsc::unbounded();

        let init_task =
            current_task.clone_task_for_test(&mut locked, CLONE_FS as u64, Some(SIGCHLD));
        const STARTUP_FILE_PATH: &str = "/path";

        let test_init_pid = init_task.get_pid();
        fasync::Task::local(async move {
            sender.send(()).await.expect("failed to send message");
            wait_for_init_file(STARTUP_FILE_PATH, &current_task, test_init_pid)
                .await
                .expect_err("Did not detect init exit");
            sender.send(()).await.expect("failed to send message");
        })
        .detach();

        // Wait for message that file check has started.
        assert!(receiver.next().await.is_some());

        // Drop the `init_task`.
        std::mem::drop(init_task);

        // Wait for the init failure to be detected.
        assert!(receiver.next().await.is_some());
    }
}
