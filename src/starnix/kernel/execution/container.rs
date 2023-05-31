// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Error};
use fidl::endpoints::{ControlHandle, RequestStream};
use fidl::AsyncChannel;
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_data as fdata;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_starnix_container as fstarcontainer;
use fuchsia_async as fasync;
use fuchsia_async::DurationExt;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect as inspect;
use fuchsia_runtime as fruntime;
use fuchsia_zircon as zx;
use fuchsia_zircon::Task as _;
use futures::channel::oneshot;
use futures::{FutureExt, StreamExt, TryStreamExt};
use runner::{get_program_string, get_program_strvec, get_value};
use starnix_kernel_config::Config;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::Arc;

use crate::auth::Credentials;
use crate::device::run_features;
use crate::execution::*;
use crate::fs::layeredfs::LayeredFs;
use crate::fs::tmpfs::TmpFs;
use crate::fs::*;
use crate::logging::{log_error, log_info};
use crate::task::*;
use crate::types::*;
use fidl::HandleBased;

/// A temporary wrapper struct that contains both a `Config` for the container, as well as optional
/// handles for the container's component controller and `/pkg` directory.
///
/// When using structured_config, the `component_controller` handle will not be set. When all
/// containers are run as components, by starnix_runner, the `component_controller` will always
/// exist.
struct ConfigWrapper {
    config: Config,

    /// The `/pkg` directory of the container.
    pkg_dir: Option<zx::Channel>,

    /// The outgoing directory of the container, used to serve protocols on behalf of the container.
    /// For example, the starnix_kernel serves a component runner in the containers' outgoing
    /// directory.
    #[allow(dead_code)]
    outgoing_dir: Option<zx::Channel>,

    /// The svc directory of the container, used to access protocols from the container.
    #[allow(dead_code)]
    svc_dir: Option<zx::Channel>,
}

impl std::ops::Deref for ConfigWrapper {
    type Target = Config;
    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

/// Returns the configuration object for the container being run by this `starnix_kernel`.
fn get_config() -> Option<ConfigWrapper> {
    if let Ok(config_bytes) = std::fs::read("/container_config/config") {
        let program_dict: fdata::Dictionary =
            fidl::unpersist(&config_bytes).expect("Failed to unpersist the program dictionary.");

        let apex_hack = get_config_strvec(&program_dict, "apex_hack").unwrap_or_default();
        let features = get_config_strvec(&program_dict, "features").unwrap_or_default();
        let init = get_config_strvec(&program_dict, "init").unwrap_or_default();
        let kernel_cmdline = get_config_str(&program_dict, "kernel_cmdline").unwrap_or_default();
        let mounts = get_config_strvec(&program_dict, "mounts").unwrap_or_default();
        let name = get_config_str(&program_dict, "name").unwrap_or_default();
        let startup_file_path =
            get_config_str(&program_dict, "startup_file_path").unwrap_or_default();

        let pkg_dir = fruntime::take_startup_handle(kernel_config::PKG_HANDLE_INFO)
            .map(zx::Channel::from_handle);
        let outgoing_dir =
            fruntime::take_startup_handle(kernel_config::CONTAINER_OUTGOING_DIR_HANDLE_INFO)
                .map(zx::Channel::from_handle);
        let svc_dir = fruntime::take_startup_handle(kernel_config::CONTAINER_SVC_HANDLE_INFO)
            .map(zx::Channel::from_handle);

        Some(ConfigWrapper {
            config: Config {
                apex_hack,
                features,
                init,
                kernel_cmdline,
                mounts,
                name,
                startup_file_path,
            },
            pkg_dir,
            outgoing_dir,
            svc_dir,
        })
    } else {
        None
    }
}

fn get_ns_entry(
    ns: &mut Option<Vec<frunner::ComponentNamespaceEntry>>,
    entry_name: &str,
) -> Option<zx::Channel> {
    ns.as_mut().and_then(|ns| {
        ns.iter_mut()
            .find(|entry| entry.path == Some(entry_name.to_string()))
            .and_then(|entry| entry.directory.take())
            .map(|dir| dir.into_channel())
    })
}

fn get_config_from_component_start_info(
    mut start_info: frunner::ComponentStartInfo,
) -> ConfigWrapper {
    let get_strvec = |key| {
        get_program_strvec(&start_info, key).map(|value| value.to_owned()).unwrap_or_default()
    };

    let get_string = |key| get_program_string(&start_info, key).unwrap_or_default().to_owned();

    let apex_hack = get_strvec("apex_hack");
    let features = get_strvec("features");
    let init = get_strvec("init");
    let kernel_cmdline = get_string("kernel_cmdline");
    let mounts = get_strvec("mounts");
    let name = get_string("name");
    let startup_file_path = get_string("startup_file_path");

    let mut ns = start_info.ns.take();
    let pkg_dir = get_ns_entry(&mut ns, "/pkg");
    let svc_dir = get_ns_entry(&mut ns, "/svc");
    let outgoing_dir = start_info.outgoing_dir.take().map(|dir| dir.into_channel());

    ConfigWrapper {
        config: Config {
            apex_hack,
            features,
            init,
            kernel_cmdline,
            mounts,
            name,
            startup_file_path,
        },
        pkg_dir,
        outgoing_dir,
        svc_dir,
    }
}

fn get_config_strvec(dict: &fdata::Dictionary, key: &str) -> Option<Vec<String>> {
    match get_value(dict, key) {
        Some(fdata::DictionaryValue::StrVec(values)) => Some(values.clone()),
        _ => None,
    }
}

fn get_config_str(dict: &fdata::Dictionary, key: &str) -> Option<String> {
    match get_value(dict, key) {
        Some(fdata::DictionaryValue::Str(string)) => Some(string.clone()),
        _ => None,
    }
}

// Creates a CString from a String. Calling this with an invalid CString will panic.
fn to_cstr(str: &str) -> CString {
    CString::new(str.to_string()).unwrap()
}

pub struct Container {
    /// The `Kernel` object that is associated with the container.
    pub kernel: Arc<Kernel>,

    /// Inspect node holding information about the state of the container.
    _node: inspect::Node,
}

/// The services that are exposed in the container component's outgoing directory.
enum ExposedServices {
    ComponentRunner(frunner::ComponentRunnerRequestStream),
    ContainerController(fstarcontainer::ControllerRequestStream),
}

type TaskResult = Result<ExitStatus, Error>;

/// Attempts to read /container_config/config and the startup handles to create a container.
///
/// Returns None if /container_config/config does not exist.
///
/// This function will be replaced with a version that starts a container from ComponentStartInfo,
/// but this function exists to make a soft transition.
pub async fn maybe_create_container_from_startup_handles() -> Result<Option<Arc<Container>>, Error>
{
    if let Some(config) = get_config() {
        let (sender, receiver) = oneshot::channel::<TaskResult>();
        let container = create_container(config, sender).await?;
        fasync::Task::spawn(async {
            let _ = receiver.await;
            // Kill the starnix_kernel job, as the kernel is expected to reboot when init exits.
            fruntime::job_default().kill().expect("Failed to kill job");
        })
        .detach();
        return Ok(Some(container));
    }
    Ok(None)
}

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
) -> Result<Arc<Container>, Error> {
    if let Some(event) = request_stream.try_next().await? {
        match event {
            frunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                let request_stream = controller.into_stream()?;
                let config = get_config_from_component_start_info(start_info);
                let (sender, receiver) = oneshot::channel::<TaskResult>();
                let container = create_container(config, sender).await?;
                fasync::Task::spawn(server_component_controller(request_stream, receiver)).detach();
                return Ok(container);
            }
        }
    }
    bail!("did not receive Start request");
}

async fn create_container(
    mut config: ConfigWrapper,
    task_complete: oneshot::Sender<TaskResult>,
) -> Result<Arc<Container>, Error> {
    trace_duration!(trace_category_starnix!(), trace_name_create_container!());
    const DEFAULT_INIT: &str = "/container/init";

    // Install container svc into the kernel namespace
    let svc_dir = if let Some(svc_dir) = config.svc_dir.take() {
        Some(fio::DirectoryProxy::new(AsyncChannel::from_channel(svc_dir)?))
    } else {
        None
    };

    let pkg_dir_proxy = fio::DirectorySynchronousProxy::new(config.pkg_dir.take().unwrap());

    let kernel = Kernel::new(
        config.name.as_bytes(),
        config.kernel_cmdline.as_bytes(),
        &config.features,
        svc_dir,
    )?;

    let node = inspect::component::inspector().root().create_child("container");
    create_container_inspect(kernel.clone(), &node);

    let mut init_task = create_init_task(&kernel, &config)?;
    let fs_context = create_fs_context(&init_task, &config, &pkg_dir_proxy)?;
    init_task.set_fs(fs_context.clone());
    kernel.kthreads.init(&kernel, fs_context)?;
    let system_task = kernel.kthreads.system_task();

    mount_filesystems(system_task, &config, &pkg_dir_proxy)?;

    // Hack to allow mounting apexes before apexd is working.
    // TODO(tbodt): Remove once apexd works.
    mount_apexes(system_task, &config)?;

    // Run all common features that were specified in the .cml.
    run_features(&config.features, system_task)
        .map_err(|e| anyhow!("Failed to initialize features: {:?}", e))?;

    let startup_file_path = if config.startup_file_path.is_empty() {
        None
    } else {
        Some(config.startup_file_path.clone())
    };

    // If there is an init binary path, run it, optionally waiting for the
    // startup_file_path to be created. The task struct is still used
    // to initialize the system up until this point, regardless of whether
    // or not there is an actual init to be run.
    let argv =
        if config.init.is_empty() { vec![DEFAULT_INIT.to_string()] } else { config.init.clone() }
            .iter()
            .map(|s| to_cstr(s))
            .collect::<Vec<_>>();

    let executable = init_task.open_file(argv[0].as_bytes(), OpenFlags::RDONLY)?;
    init_task.exec(executable, argv[0].clone(), argv.clone(), vec![])?;
    execute_task(init_task, move |result| {
        log_info!("Finished running init process: {:?}", result);
        let _ = task_complete.send(result);
    });
    if let Some(startup_file_path) = startup_file_path {
        wait_for_init_file(&startup_file_path, system_task).await?;
    };

    let container = Arc::new(Container { kernel, _node: node });

    if let Some(outgoing_dir_channel) = config.outgoing_dir.take() {
        let container_clone = container.clone();
        // Add `ComponentRunner` to the exposed services of the container, and then serve the
        // outgoing directory.
        let mut outgoing_directory = ServiceFs::new_local();
        outgoing_directory
            .dir("svc")
            .add_fidl_service(ExposedServices::ComponentRunner)
            .add_fidl_service(ExposedServices::ContainerController);
        outgoing_directory
            .serve_connection(outgoing_dir_channel.into())
            .map_err(|_| errno!(EINVAL))?;

        fasync::Task::local(async move {
            let container_clone = container_clone.clone();
            while let Some(request_stream) = outgoing_directory.next().await {
                let container_clone = container_clone.clone();
                match request_stream {
                    ExposedServices::ComponentRunner(request_stream) => {
                        fasync::Task::local(async move {
                            match serve_component_runner(request_stream, container_clone.clone())
                                .await
                            {
                                Ok(_) => {}
                                Err(e) => {
                                    log_error!("Error serving component runner: {:?}", e);
                                }
                            }
                        })
                        .detach();
                    }
                    ExposedServices::ContainerController(request_stream) => {
                        fasync::Task::local(async move {
                            serve_container_controller(request_stream, container_clone.clone())
                                .await
                                .expect("failed to start container.")
                        })
                        .detach();
                    }
                }
            }
        })
        .detach();
    }

    Ok(container)
}

fn create_fs_context(
    task: &CurrentTask,
    config: &ConfigWrapper,
    pkg_dir_proxy: &fio::DirectorySynchronousProxy,
) -> Result<Arc<FsContext>, Error> {
    // The mounts are appplied in the order listed. Mounting will fail if the designated mount
    // point doesn't exist in a previous mount. The root must be first so other mounts can be
    // applied on top of it.
    let mut mounts_iter = config.mounts.iter();
    let (root_point, root_fs) = create_filesystem_from_spec(
        task,
        pkg_dir_proxy,
        mounts_iter.next().ok_or_else(|| anyhow!("Mounts list is empty"))?,
    )?;
    if root_point != b"/" {
        anyhow::bail!("First mount in mounts list is not the root");
    }
    let root_fs = if let WhatToMount::Fs(fs) = root_fs {
        fs
    } else {
        anyhow::bail!("how did a bind mount manage to get created as the root?")
    };

    // Create a layered fs to handle /container and /container/component
    // /container will mount the container pkg
    // /container/component will be a tmpfs where component using the starnix kernel will have their
    // package mounted.
    let kernel = task.kernel();
    let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;
    let container_fs = LayeredFs::new_fs(
        kernel,
        create_remotefs_filesystem(kernel, pkg_dir_proxy, rights, "data")?,
        BTreeMap::from([(b"component".to_vec(), TmpFs::new_fs(kernel))]),
    );
    let mut mappings =
        vec![(b"container".to_vec(), container_fs), (b"data".to_vec(), TmpFs::new_fs(kernel))];
    if config.features.contains(&"custom_artifacts".to_string()) {
        mappings.push((b"custom_artifacts".to_vec(), TmpFs::new_fs(kernel)));
    }
    if config.features.contains(&"test_data".to_string()) {
        mappings.push((b"test_data".to_vec(), TmpFs::new_fs(kernel)));
    }
    let root_fs = LayeredFs::new_fs(kernel, root_fs, mappings.into_iter().collect());

    Ok(FsContext::new(root_fs))
}

fn mount_apexes(init_task: &CurrentTask, config: &ConfigWrapper) -> Result<(), Error> {
    if !config.apex_hack.is_empty() {
        init_task
            .lookup_path_from_root(b"apex")?
            .mount(WhatToMount::Fs(TmpFs::new_fs(init_task.kernel())), MountFlags::empty())?;
        let apex_dir = init_task.lookup_path_from_root(b"apex")?;
        for apex in &config.apex_hack {
            let apex = apex.as_bytes();
            let apex_subdir =
                apex_dir.create_node(init_task, apex, mode!(IFDIR, 0o700), DeviceType::NONE)?;
            let apex_source = init_task.lookup_path_from_root(&[b"system/apex/", apex].concat())?;
            apex_subdir.mount(WhatToMount::Bind(apex_source), MountFlags::empty())?;
        }
    }
    Ok(())
}

fn create_init_task(kernel: &Arc<Kernel>, config: &ConfigWrapper) -> Result<CurrentTask, Error> {
    let credentials = Credentials::root();
    let initial_name = if config.init.is_empty() {
        CString::default()
    } else {
        CString::new(config.init[0].clone())?
    };
    let task = Task::create_process_without_parent(kernel, initial_name, None)?;
    task.set_creds(credentials);
    Ok(task)
}

fn mount_filesystems(
    system_task: &CurrentTask,
    config: &ConfigWrapper,
    pkg_dir_proxy: &fio::DirectorySynchronousProxy,
) -> Result<(), Error> {
    let mut mounts_iter = config.mounts.iter();
    // Skip the first mount, that was used to create the root filesystem.
    let _ = mounts_iter.next();
    for mount_spec in mounts_iter {
        let (mount_point, child_fs) =
            create_filesystem_from_spec(system_task, pkg_dir_proxy, mount_spec)?;
        let mount_point = system_task.lookup_path_from_root(mount_point)?;
        mount_point.mount(child_fs, MountFlags::empty())?;
    }
    Ok(())
}

async fn wait_for_init_file(
    startup_file_path: &str,
    current_task: &CurrentTask,
) -> Result<(), Error> {
    // TODO(fxb/96299): Use inotify machinery to wait for the file.
    loop {
        fasync::Timer::new(fasync::Duration::from_millis(100).after_now()).await;
        let root = current_task.fs().root();
        let mut context = LookupContext::default();
        match current_task.lookup_path(&mut context, root, startup_file_path.as_bytes()) {
            Ok(_) => break,
            Err(error) if error == ENOENT => continue,
            Err(error) => return Err(anyhow::Error::from(error)),
        }
    }
    Ok(())
}

/// Creates a lazy node that will contain the Kernel thread groups state.
fn create_container_inspect(kernel: Arc<Kernel>, parent: &inspect::Node) {
    parent.record_lazy_child("kernel", move || {
        let inspector = inspect::Inspector::default();
        let thread_groups = inspector.root().create_child("thread_groups");
        for thread_group in kernel.pids.read().get_thread_groups() {
            let tg = thread_group.read();

            let tg_node = thread_groups.create_child(format!("{}", thread_group.leader));
            tg_node.record_int("ppid", tg.get_ppid() as i64);
            tg_node.record_bool("stopped", tg.stopped);

            let tasks_node = tg_node.create_child("tasks");
            for task in tg.tasks() {
                if task.id == thread_group.leader {
                    record_task_command_to_node(&task, "command", &tg_node);
                    continue;
                }
                record_task_command_to_node(&task, format!("{}", task.id), &tasks_node);
            }
            tg_node.record(tasks_node);
            thread_groups.record(tg_node);
        }
        inspector.root().record(thread_groups);

        async move { Ok(inspector) }.boxed()
    });
}

fn record_task_command_to_node(
    task: &Arc<Task>,
    name: impl Into<inspect::StringReference>,
    node: &inspect::Node,
) {
    match task.command().to_str() {
        Ok(command) => node.record_string(name, command),
        Err(err) => node.record_string(name, format!("{err}")),
    }
}

#[cfg(test)]
mod test {
    use super::wait_for_init_file;
    use crate::fs::FdNumber;
    use crate::testing::create_kernel_and_task;
    use crate::types::*;
    use fuchsia_async as fasync;
    use futures::{SinkExt, StreamExt};

    #[fuchsia::test]
    async fn test_init_file_already_exists() {
        let (_kernel, current_task) = create_kernel_and_task();
        let (mut sender, mut receiver) = futures::channel::mpsc::unbounded();

        let path = "/path";
        current_task
            .open_file_at(
                FdNumber::AT_FDCWD,
                path.as_bytes(),
                OpenFlags::CREAT,
                FileMode::default(),
            )
            .expect("Failed to create file");

        fasync::Task::local(async move {
            wait_for_init_file(path, &current_task).await.expect("failed to wait for file");
            sender.send(()).await.expect("failed to send message");
        })
        .detach();

        // Wait for the file creation to have been detected.
        assert!(receiver.next().await.is_some());
    }

    #[fuchsia::test]
    async fn test_init_file_wait_required() {
        let (_kernel, current_task) = create_kernel_and_task();
        let (mut sender, mut receiver) = futures::channel::mpsc::unbounded();

        let init_task = current_task.clone_task_for_test(CLONE_FS as u64, Some(SIGCHLD));
        let path = "/path";

        fasync::Task::local(async move {
            sender.send(()).await.expect("failed to send message");
            wait_for_init_file(path, &init_task).await.expect("failed to wait for file");
            sender.send(()).await.expect("failed to send message");
        })
        .detach();

        // Wait for message that file check has started.
        assert!(receiver.next().await.is_some());

        // Create the file that is being waited on.
        current_task
            .open_file_at(
                FdNumber::AT_FDCWD,
                path.as_bytes(),
                OpenFlags::CREAT,
                FileMode::default(),
            )
            .expect("Failed to create file");

        // Wait for the file creation to be detected.
        assert!(receiver.next().await.is_some());
    }
}
