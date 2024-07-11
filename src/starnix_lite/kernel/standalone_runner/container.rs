// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{parse_features, run_container_features, Features};
use anyhow::{anyhow, bail, Error};
use bstr::BString;
use fidl::AsyncChannel;
use fidl_fuchsia_scheduler::RoleManagerSynchronousProxy;
use fuchsia_async::DurationExt;
use fuchsia_zircon::{
    Task as _, {self as zx},
};
use futures::channel::oneshot;
use futures::{FutureExt, StreamExt};
use selinux::security_server::SecurityServer;
use starnix_core::device::init_common_devices;
use starnix_core::execution::{
    create_filesystem_from_spec, create_remotefs_filesystem, execute_task_with_prerun_result,
    parse_numbered_handles,
};
use starnix_core::fs::layeredfs::LayeredFs;
use starnix_core::fs::overlayfs::OverlayFs;
use starnix_core::fs::tmpfs::TmpFs;
use starnix_core::task::{CurrentTask, ExitStatus, Kernel, Task};
use starnix_core::vfs::{FileSystemOptions, FsContext, LookupContext, WhatToMount};
use starnix_lite_kernel_config::Config;
use starnix_logging::{
    log_error, log_info, log_warn, trace_duration, CATEGORY_STARNIX, NAME_CREATE_CONTAINER,
};
use starnix_sync::{BeforeFsNodeAppend, DeviceOpen, FileOpsCore, LockBefore, Locked};
use starnix_uapi::errors::{SourceContext, ENOENT};
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::resource_limits::Resource;
use starnix_uapi::{errno, rlimit};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::ops::DerefMut;
use std::sync::Arc;
use {
    fidl_fuchsia_io as fio, fuchsia_async as fasync, fuchsia_inspect as inspect,
    fuchsia_runtime as fruntime,
};

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

    /// The svc directory of the container, used to access protocols from the container.
    svc_dir: Option<zx::Channel>,

    /// The data directory of the container, used to persist data.
    data_dir: Option<zx::Channel>,
}

impl std::ops::Deref for ConfigWrapper {
    type Target = Config;
    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

// Creates a CString from a String. Calling this with an invalid CString will panic.
fn to_cstr(str: &str) -> CString {
    CString::new(str.to_string()).unwrap()
}

#[must_use = "The container must run serve on this config"]
pub struct ContainerServiceConfig {
    receiver: oneshot::Receiver<Result<ExitStatus, Error>>,
}

pub struct Container {
    /// The `Kernel` object that is associated with the container.
    pub kernel: Arc<Kernel>,

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

    pub async fn run(&self, service_config: ContainerServiceConfig) -> Result<(), Error> {
        let _ = futures::join!(run_container_controller(service_config.receiver));
        Ok(())
    }
}

type TaskResult = Result<ExitStatus, Error>;

async fn run_container_controller(task_complete: oneshot::Receiver<TaskResult>) {
    enum Event<T> {
        Completion(T),
    }

    if let Some(event) = task_complete.into_stream().map(Event::Completion).next().await {
        match event {
            Event::Completion(result) => {
                match result {
                    Ok(Ok(ExitStatus::Exit(0))) => {
                        log_info!("Exit gracefully")
                    }
                    _ => log_warn!("Something went wrong"),
                };
            }
        }
    }

    // Kill the starnix_kernel job, as the kernel is expected to reboot when init exits.
    fruntime::job_default().kill().expect("Failed to kill job");
}

fn open_boot() -> zx::Channel {
    let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;
    let (server, client) = zx::Channel::create();
    fdio::open("/boot", rights, server).expect("failed to open /boot");
    client
}

pub async fn create_container_from_config(
    config: Config,
) -> Result<(Container, ContainerServiceConfig), Error> {
    let mut config_wrapper =
        ConfigWrapper { config: config, pkg_dir: Some(open_boot()), svc_dir: None, data_dir: None };

    let (sender, receiver) = oneshot::channel::<TaskResult>();
    let container =
        create_container(&mut config_wrapper, sender).await.with_source_context(|| {
            format!("creating container \"{}\"", &config_wrapper.config.name)
        })?;
    let service_config = ContainerServiceConfig { receiver };
    return Ok((container, service_config));
}

async fn create_container(
    config: &mut ConfigWrapper,
    task_complete: oneshot::Sender<TaskResult>,
) -> Result<Container, Error> {
    trace_duration!(CATEGORY_STARNIX, NAME_CREATE_CONTAINER);
    const DEFAULT_INIT: &str = "/container/init";

    // Install container svc into the kernel namespace
    let svc_dir = if let Some(svc_dir) = config.svc_dir.take() {
        Some(fio::DirectoryProxy::new(AsyncChannel::from_channel(svc_dir)))
    } else {
        None
    };

    let data_dir = if let Some(data_dir) = config.data_dir.take() {
        Some(fio::DirectorySynchronousProxy::new(data_dir))
    } else {
        None
    };

    let pkg_dir_proxy = fio::DirectorySynchronousProxy::new(config.pkg_dir.take().unwrap());

    let features = parse_features(&config.features)?;

    let kernel_cmdline = BString::from(config.kernel_cmdline.as_bytes());

    // Check whether we actually have access to a role manager by trying to set our own
    // thread's role.
    let role_manager: Option<RoleManagerSynchronousProxy> = None;

    let node = inspect::component::inspector().root().create_child("container");
    let security_server = match features.selinux {
        Some(mode) => Some(SecurityServer::new(mode)),
        _ => None,
    };
    let kernel = Kernel::new(
        kernel_cmdline,
        features.kernel,
        svc_dir,
        data_dir,
        role_manager,
        node.create_child("kernel"),
        security_server,
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

    if let Err(e) = kernel.suspend_resume_manager.init(&system_task) {
        log_warn!("Suspend/Resume manager initialization failed: ({e:?})");
    }

    // Register common devices and add them in sysfs and devtmpfs.
    init_common_devices(kernel.kthreads.unlocked_for_async().deref_mut(), &system_task);

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
            parse_numbered_handles(init_task, None, &init_task.files).map_err(|e| {
                log_error!("Error while parsing the numbered handles: {e:?}");
                errno!(EINVAL)
            })?;

            init_task.exec(locked, executable, argv[0].clone(), argv.clone(), vec![])
        },
        move |result| {
            log_info!("Finished running init process: {:?}", result);
            let _ = task_complete.send(result);
        },
        None,
    )?;

    if !config.startup_file_path.is_empty() {
        wait_for_init_file(&config.startup_file_path, &system_task).await?;
    };

    Ok(Container { kernel, _node: node, _thread_bound: Default::default() })
}

fn create_fs_context<L>(
    locked: &mut Locked<'_, L>,
    kernel: &Arc<Kernel>,
    features: &Features,
    config: &ConfigWrapper,
    pkg_dir_proxy: &fio::DirectorySynchronousProxy,
) -> Result<Arc<FsContext>, Error>
where
    L: LockBefore<FileOpsCore>,
    L: LockBefore<DeviceOpen>,
    L: LockBefore<BeforeFsNodeAppend>,
{
    // The mounts are applied in the order listed. Mounting will fail if the designated mount
    // point doesn't exist in a previous mount. The root must be first so other mounts can be
    // applied on top of it.
    let mut mounts_iter = config.mounts.iter();
    let (root_point, mut root_fs) = create_filesystem_from_spec(
        locked,
        kernel,
        pkg_dir_proxy,
        mounts_iter.next().ok_or_else(|| anyhow!("Mounts list is empty"))?,
    )?;
    if root_point != "/" {
        anyhow::bail!("First mount in mounts list is not the root");
    }

    // Create a layered fs to handle /container and /container/component
    let mut mappings = vec![];
    if features.container {
        // /container will mount the container pkg
        // /container/component will be a tmpfs where component using the starnix kernel will have their
        // package mounted.
        let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;
        let container_fs = LayeredFs::new_fs(
            kernel,
            create_remotefs_filesystem(
                kernel,
                pkg_dir_proxy,
                rights,
                FileSystemOptions { source: "data".into(), ..Default::default() },
            )?,
            BTreeMap::from([("component".into(), TmpFs::new_fs(kernel))]),
        );
        mappings.push(("container".into(), container_fs));
    }
    if features.test_data {
        mappings.push(("test_data".into(), TmpFs::new_fs(kernel)));
    }

    if !mappings.is_empty() {
        // If this container has enabled any features that mount directories that might not exist
        // in the root file system, we add a LayeredFs to hold these mappings.
        root_fs = LayeredFs::new_fs(kernel, root_fs, mappings.into_iter().collect());
    }
    if features.rootfs_rw {
        root_fs = OverlayFs::wrap_fs_in_writable_layer(kernel, root_fs)?;
    }
    Ok(FsContext::new(root_fs))
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

fn mount_filesystems<L>(
    locked: &mut Locked<'_, L>,
    system_task: &CurrentTask,
    config: &ConfigWrapper,
    pkg_dir_proxy: &fio::DirectorySynchronousProxy,
) -> Result<(), Error>
where
    L: LockBefore<FileOpsCore>,
    L: LockBefore<DeviceOpen>,
    L: LockBefore<BeforeFsNodeAppend>,
{
    let mut mounts_iter = config.mounts.iter();
    // Skip the first mount, that was used to create the root filesystem.
    let _ = mounts_iter.next();
    for mount_spec in mounts_iter {
        let (mount_point, child_fs) =
            create_filesystem_from_spec(locked, system_task, pkg_dir_proxy, mount_spec)
                .with_source_context(|| {
                    format!("creating filesystem from spec: {}", &mount_spec)
                })?;
        let mount_point = system_task
            .lookup_path_from_root(mount_point)
            .with_source_context(|| format!("lookup path from root: {mount_point}"))?;
        mount_point.mount(WhatToMount::Fs(child_fs), MountFlags::empty())?;
    }
    Ok(())
}

async fn wait_for_init_file(
    startup_file_path: &str,
    current_task: &CurrentTask,
) -> Result<(), Error> {
    // TODO(https://fxbug.dev/42178400): Use inotify machinery to wait for the file.
    loop {
        fasync::Timer::new(fasync::Duration::from_millis(100).after_now()).await;
        let root = current_task.fs().root();
        let mut context = LookupContext::default();
        match current_task.lookup_path(&mut context, root, startup_file_path.into()) {
            Ok(_) => break,
            Err(error) if error == ENOENT => continue,
            Err(error) => return Err(anyhow::Error::from(error)),
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
    use starnix_uapi::file_mode::FileMode;
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
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (mut sender, mut receiver) = futures::channel::mpsc::unbounded();

        let init_task =
            current_task.clone_task_for_test(&mut locked, CLONE_FS as u64, Some(SIGCHLD));
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
                &mut locked,
                FdNumber::AT_FDCWD,
                path.into(),
                OpenFlags::CREAT,
                FileMode::default(),
                ResolveFlags::empty(),
            )
            .expect("Failed to create file");

        // Wait for the file creation to be detected.
        assert!(receiver.next().await.is_some());
    }
}
