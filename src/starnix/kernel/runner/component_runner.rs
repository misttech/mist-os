// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{run_component_features, MountAction};
use anyhow::{anyhow, bail, Context, Error};
use fidl::endpoints::{ControlHandle, RequestStream, ServerEnd};
use fidl::AsyncChannel;
use fidl_fuchsia_component_runner::{
    ComponentControllerMarker, ComponentControllerRequest, ComponentControllerRequestStream,
    ComponentStartInfo,
};
use fuchsia_runtime::{HandleInfo, HandleType};
use futures::channel::oneshot;
use futures::{FutureExt, StreamExt};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use runner::{get_program_string, get_program_strvec, StartInfoProgramError};
use starnix_core::execution::execute_task_with_prerun_result;
use starnix_core::fs::fuchsia::{create_file_from_handle, RemoteFs, SyslogFile};
use starnix_core::task::{CurrentTask, ExitStatus, Task};
use starnix_core::vfs::fs_args::MountParams;
use starnix_core::vfs::{
    FdNumber, FdTable, FileSystemOptions, FsString, LookupContext, NamespaceNode, WhatToMount,
};
use starnix_core::{security, signals};
use starnix_logging::{log_error, log_info, log_warn};
use starnix_sync::{FileOpsCore, LockBefore, Locked, Mutex};
use starnix_types::ownership::WeakRef;
use starnix_uapi::auth::{Capabilities, Credentials};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errno;
use starnix_uapi::errors::{Errno, EEXIST, ENOTDIR};
use starnix_uapi::file_mode::mode;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::signals::{SIGINT, SIGKILL};
use starnix_uapi::unmount_flags::UnmountFlags;
use std::ffi::CString;
use std::ops::DerefMut;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio, fidl_fuchsia_process as fprocess,
    fuchsia_async as fasync,
};

/// Component controller epitaph value used as the base value to pass non-zero error
/// codes to the calling component.
///
/// TODO(https://fxbug.dev/42081234): Cleanup this once we have a proper mechanism to
/// get Linux exit code from component runner.
const COMPONENT_EXIT_CODE_BASE: i32 = 1024;

/// Starts a component inside the given container.
///
/// The component's `binary` can either:
///   - an absolute path, in which case the path is treated as a path into the root filesystem that
///     is mounted by the container's configuration
///   - relative path, in which case the binary is read from the component's package (which is
///     mounted at /container/component/{random}/pkg.)
///
/// The directories in the component's namespace are mounted at /container/component/{random}.
pub async fn start_component(
    mut start_info: ComponentStartInfo,
    controller: ServerEnd<ComponentControllerMarker>,
    system_task: &CurrentTask,
) -> Result<(), Error> {
    let url = start_info.resolved_url.clone().unwrap_or_else(|| "<unknown>".to_string());
    log_info!("start_component: {}", url);

    // TODO(https://fxbug.dev/42076551): We leak the directory created by this function.
    let component_path = generate_component_path(
        system_task.kernel().kthreads.unlocked_for_async().deref_mut(),
        system_task,
    )?;

    let mount_record = Arc::new(Mutex::new(MountRecord::default()));

    let ns = start_info.ns.take().ok_or_else(|| anyhow!("Missing namespace"))?;

    // If the component specifies a filesystem security label then it will be applied to all files
    // in all directories mounted from the component's namespace.
    let kernel = system_task.kernel();
    let mount_seclabel = {
        match get_program_string(&start_info, "fsseclabel") {
            Some(s) => Some(s),
            None => kernel.features.default_fsseclabel.as_deref(),
        }
    };

    let mut maybe_pkg = None;
    let mut maybe_svc = None;
    for entry in ns {
        if let (Some(dir_path), Some(dir_handle)) = (entry.path, entry.directory) {
            match dir_path.as_str() {
                "/svc" => {
                    maybe_svc = Some(fio::DirectoryProxy::new(AsyncChannel::from_channel(
                        dir_handle.into_channel(),
                    )));
                }
                "/custom_artifacts" | "/test_data" => {
                    // Mount custom_artifacts and test_data directory at root of container
                    // We may want to transition to have these directories unique per component
                    let dir_proxy = fio::DirectorySynchronousProxy::new(dir_handle.into_channel());
                    mount_record
                        .lock()
                        .mount_remote(
                            system_task.kernel().kthreads.unlocked_for_async().deref_mut(),
                            system_task,
                            &dir_proxy,
                            &dir_path,
                            mount_seclabel,
                        )
                        .with_context(|| format!("failed to mount_remote on path {}", &dir_path))?;
                }
                _ => {
                    let dir_proxy = fio::DirectorySynchronousProxy::new(dir_handle.into_channel());
                    mount_record
                        .lock()
                        .mount_remote(
                            system_task.kernel().kthreads.unlocked_for_async().deref_mut(),
                            system_task,
                            &dir_proxy,
                            &format!("{component_path}/{dir_path}"),
                            mount_seclabel,
                        )
                        .with_context(|| {
                            format!("failed to mount_remote on path {component_path}/{dir_path}")
                        })?;
                    if dir_path == "/pkg" {
                        maybe_pkg = Some(dir_proxy);
                    }
                }
            }
        }
    }

    let pkg = maybe_pkg.ok_or_else(|| anyhow!("Missing /pkg entry in namespace"))?;
    let pkg_path = format!("{component_path}/pkg");

    let resolve_template = |value: &str| {
        value.replace("{pkg_path}", &pkg_path).replace("{component_path}", &component_path)
    };

    let resolve_program_strvec = |key| {
        get_program_strvec(&start_info, key)?
            .unwrap_or(&vec![])
            .iter()
            .map(|arg| {
                CString::new(resolve_template(arg))
                    .map_err(|_| StartInfoProgramError::InvalidStrVec(key.to_string()))
            })
            .collect::<Result<Vec<CString>, _>>()
    };

    let args = resolve_program_strvec("args")?;
    let environ = resolve_program_strvec("environ")?;
    let component_features =
        get_program_strvec(&start_info, "features")?.cloned().unwrap_or_default();

    let binary_path = get_program_string(&start_info, "binary")
        .ok_or_else(|| anyhow!("Missing \"binary\" in manifest"))?;
    let binary_path = CString::new(binary_path.to_owned())?;

    let uid = {
        if let Some(component_uid) = get_program_string(&start_info, "uid") {
            component_uid.parse()?
        } else {
            system_task.kernel().features.default_uid
        }
    };
    let mut credentials = Credentials::with_ids(uid, uid);
    if let Some(caps) = get_program_strvec(&start_info, "capabilities")? {
        let mut capabilities = Capabilities::empty();
        for cap in caps {
            capabilities |= cap.parse()?;
        }
        credentials.cap_permitted = capabilities;
        credentials.cap_effective = capabilities;
        credentials.cap_inheritable = capabilities;
        credentials.cap_ambient = capabilities;
    }

    let kernel = system_task.kernel();
    run_component_features(&kernel, &component_features, maybe_svc).unwrap_or_else(|e| {
        log_error!("failed to set component features for {} - {:?}", url, e);
    });

    let security_context = {
        match get_program_string(&start_info, "seclabel") {
            Some(s) => Some(CString::new(s.to_owned())?),
            None => system_task
                .kernel()
                .features
                .default_seclabel
                .as_ref()
                .map(|s| CString::new(s.clone()).expect("seclabel cstring")),
        }
    };
    let (task_complete_sender, task_complete) = oneshot::channel::<TaskResult>();
    let kernel = system_task.kernel();
    let current_task = CurrentTask::create_init_child_process(
        system_task.kernel().kthreads.unlocked_for_async().deref_mut(),
        &kernel,
        &binary_path,
        security_context.as_ref(),
    )?;

    let weak_task = execute_task_with_prerun_result(
        system_task.kernel().kthreads.unlocked_for_async().deref_mut(),
        current_task,
        {
            let mount_record = mount_record.clone();
            move |locked, current_task| {
                let cwd_path =
                    FsString::from(get_program_string(&start_info, "cwd").unwrap_or(&pkg_path));
                let cwd = current_task.lookup_path(
                    locked,
                    &mut LookupContext::default(),
                    current_task.fs().root(),
                    cwd_path.as_ref(),
                )?;
                current_task.fs().chdir(locked, current_task, cwd)?;

                current_task.set_creds(credentials);

                let local_mounts =
                    get_program_strvec(&start_info, "component_mounts").map_err(|e| {
                        log_error!("Error while reading the mounts: {e:?}");
                        errno!(EINVAL)
                    })?;
                if let Some(local_mounts) = local_mounts {
                    for mount in local_mounts.iter() {
                        let action = MountAction::from_spec(locked, current_task, &pkg, mount)
                            .map_err(|e| {
                                log_error!("Error while mounting the filesystems: {e:?}");
                                errno!(EINVAL)
                            })?;
                        let mount_point =
                            current_task.lookup_path_from_root(locked, action.path.as_ref())?;
                        mount_record.lock().mount(
                            mount_point,
                            WhatToMount::Fs(action.fs),
                            action.flags,
                        )?;
                    }
                }

                parse_numbered_handles(
                    current_task,
                    start_info.numbered_handles,
                    &current_task.files,
                )
                .map_err(|e| {
                    log_error!("Error while parsing the numbered handles: {e:?}");
                    errno!(EINVAL)
                })?;

                let mut argv = vec![binary_path.clone()];
                argv.extend(args);

                let executable = current_task.open_file(
                    locked,
                    binary_path.as_bytes().into(),
                    OpenFlags::RDONLY,
                )?;
                current_task.exec(locked, executable, binary_path, argv, environ)?;

                Ok(WeakRef::from(&current_task.task))
            }
        },
        move |result| {
            // If the component controller server has gone away, there is nobody for us to
            // report the result to.
            let _ = task_complete_sender.send(result);
            // Unmount all the directories for this component.
            std::mem::drop(mount_record);
        },
        None,
    )?;
    let controller = controller.into_stream();
    fasync::Task::local(serve_component_controller(controller, weak_task, task_complete)).detach();

    Ok(())
}

type TaskResult = Result<ExitStatus, Error>;

/// Translates [ComponentControllerRequest] messages to signals on the `task`.
///
/// When a `Stop` request is received, it will send a `SIGINT` to the task.
/// When a `Kill` request is received, it will send a `SIGKILL` to the task and close the component
/// controller channel regardless if/how the task responded to the signal. Due to Linux's design,
/// this may not reliably cleanup everything that was started as a result of running the component.
///
/// If the task has completed, it will also close the controller channel.
async fn serve_component_controller(
    controller: ComponentControllerRequestStream,
    task: WeakRef<Task>,
    task_complete: oneshot::Receiver<TaskResult>,
) {
    let controller_handle = controller.control_handle();

    enum Event<T, U> {
        Controller(T),
        Completion(U),
    }

    let mut stream = futures::stream::select(
        controller.map(Event::Controller),
        task_complete.into_stream().map(Event::Completion),
    );

    while let Some(event) = stream.next().await {
        match event {
            Event::Controller(request) => match request {
                Ok(ComponentControllerRequest::Stop { .. }) => {
                    if let Some(task) = task.upgrade() {
                        signals::send_standard_signal(
                            task.as_ref(),
                            signals::SignalInfo::default(SIGINT),
                        );
                        log_info!("Sent SIGINT to program {:}", task.command().to_string_lossy());
                    }
                }
                Ok(ComponentControllerRequest::Kill { .. }) => {
                    if let Some(task) = task.upgrade() {
                        signals::send_standard_signal(&task, signals::SignalInfo::default(SIGKILL));
                        log_info!("Sent SIGKILL to program {:}", task.command().to_string_lossy());
                        controller_handle.shutdown_with_epitaph(zx::Status::from_raw(
                            fcomponent::Error::InstanceDied.into_primitive() as i32,
                        ));
                    }
                    return;
                }
                Ok(ComponentControllerRequest::_UnknownMethod { ordinal, .. }) => {
                    log_warn!("Unknown ComponentController request: {ordinal}");
                }
                Err(_) => {
                    return;
                }
            },
            Event::Completion(result) => match result {
                Ok(Ok(ExitStatus::Exit(0))) => {
                    controller_handle.shutdown_with_epitaph(zx::Status::OK)
                }
                Ok(Ok(ExitStatus::Exit(n))) => controller_handle.shutdown_with_epitaph(
                    zx::Status::from_raw(COMPONENT_EXIT_CODE_BASE + n as i32),
                ),
                _ => controller_handle.shutdown_with_epitaph(zx::Status::from_raw(
                    fcomponent::Error::InstanceDied.into_primitive() as i32,
                )),
            },
        }
    }
}

/// Returns /container/component/{random} that doesn't already exist
fn generate_component_path<L>(
    locked: &mut Locked<'_, L>,
    system_task: &CurrentTask,
) -> Result<String, Error>
where
    L: LockBefore<FileOpsCore>,
{
    // Checking container directory already exists.
    // If this lookup fails, the container might not have the "container" feature enabled.
    let mount_point = system_task.lookup_path_from_root(locked, "/container/component/".into())?;

    // Find /container/component/{random} that doesn't already exist
    let component_path = loop {
        let random_string: String =
            thread_rng().sample_iter(&Alphanumeric).take(10).map(char::from).collect();

        // This returns EEXIST if /container/component/{random} already exists.
        // If so, try again with another {random} string.
        match mount_point.create_node(
            locked,
            system_task,
            random_string.as_str().into(),
            mode!(IFDIR, 0o755),
            DeviceType::NONE,
        ) {
            Ok(_) => break format!("/container/component/{random_string}"),
            Err(errno) if errno == EEXIST => {}
            Err(e) => bail!(e),
        };
    };

    Ok(component_path)
}

/// Adds the given startup handles to a CurrentTask.
///
/// The `numbered_handles` of type `HandleType::FileDescriptor` are used to
/// create files, and the handles are required to be of type `zx::Socket`.
///
/// If there is a `numbered_handles` of type `HandleType::User0`, that is
/// interpreted as the server end of the ShellController protocol.
pub fn parse_numbered_handles(
    current_task: &CurrentTask,
    numbered_handles: Option<Vec<fprocess::HandleInfo>>,
    files: &FdTable,
) -> Result<(), Error> {
    if let Some(numbered_handles) = numbered_handles {
        for numbered_handle in numbered_handles {
            let info = HandleInfo::try_from(numbered_handle.id)?;
            if info.handle_type() == HandleType::FileDescriptor {
                files.insert(
                    current_task,
                    FdNumber::from_raw(info.arg().into()),
                    create_file_from_handle(current_task, numbered_handle.handle)?,
                )?;
            }
        }
    }

    let stdio = SyslogFile::new_file(current_task);
    // If no numbered handle is provided for each stdio handle, default to syslog.
    for i in [0, 1, 2] {
        if files.get(FdNumber::from_raw(i)).is_err() {
            files.insert(current_task, FdNumber::from_raw(i), stdio.clone())?;
        }
    }

    Ok(())
}

/// A record of the mounts created when starting a component.
///
/// When the record is dropped, the mounts are unmounted.
#[derive(Default)]
struct MountRecord {
    /// The namespace nodes at which we have crated mounts for this component.
    mounts: Vec<NamespaceNode>,
}

impl MountRecord {
    fn mount(
        &mut self,
        mount_point: NamespaceNode,
        what: WhatToMount,
        flags: MountFlags,
    ) -> Result<(), Errno> {
        mount_point.mount(what, flags)?;
        self.mounts.push(mount_point);
        Ok(())
    }

    fn mount_remote<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        system_task: &CurrentTask,
        directory: &fio::DirectorySynchronousProxy,
        path: &str,
        mount_seclabel: Option<&str>,
    ) -> Result<(), Error>
    where
        L: LockBefore<FileOpsCore>,
    {
        // The incoming dir_path might not be top level, e.g. it could be /foo/bar.
        // Iterate through each component directory starting from the parent and
        // create it if it doesn't exist.
        let mut current_node =
            system_task.lookup_path_from_root(locked, ".".into()).context("looking up '.'")?;
        let mut context = LookupContext::default();

        // Extract each component using Path::new(path).components(). For example,
        // Path::new("/foo/bar").components() will return [RootDir, Normal("foo"), Normal("bar")].
        // We're not interested in the RootDir, so we drop the prefix "/" if it exists.
        let path = if let Some(path) = path.strip_prefix('/') { path } else { path };

        for sub_dir in Path::new(path).components() {
            let sub_dir_bytes = sub_dir.as_os_str().as_bytes();
            current_node = match current_node.create_node(
                locked,
                system_task,
                sub_dir_bytes.into(),
                mode!(IFDIR, 0o755),
                DeviceType::NONE,
            ) {
                Ok(node) => node,
                Err(errno) if errno == EEXIST || errno == ENOTDIR => current_node
                    .lookup_child(locked, system_task, &mut context, sub_dir_bytes.into())
                    .with_context(|| format!("looking up {sub_dir:?}"))?,
                Err(e) => bail!(e),
            };
        }

        // TODO(https://fxbug.dev/376509077): Migrate this to GetFlags2 when available.
        let info = directory
            .get_connection_info(zx::MonotonicInstant::INFINITE)
            .context("getting directory connection info")?;
        let rights = fio::Flags::from_bits(info.rights.unwrap().bits()).unwrap();

        let (client_end, server_end) = zx::Channel::create();
        directory.clone(ServerEnd::new(server_end)).context("cloning directory")?;

        // If a filesystem security label argument was provided then apply it to all files via
        // mountpoint-labeling, with a "context=..." mount option.
        let params = if let Some(security_context) = mount_seclabel {
            MountParams::parse(format!("context={}", security_context).as_str().into()).unwrap()
        } else {
            MountParams::default()
        };

        let kernel = system_task.kernel();
        let fs = RemoteFs::new_fs(
            &kernel,
            client_end,
            FileSystemOptions { source: path.into(), params, ..Default::default() },
            rights,
        )
        .context("making remote fs")?;

        security::file_system_resolve_security(locked, system_task, &fs)
            .context("resolving security")?;

        // Fuchsia doesn't specify mount flags in the incoming namespace, so we need to make
        // up some flags.
        let flags = MountFlags::NOSUID | MountFlags::NODEV | MountFlags::RELATIME;
        current_node.mount(WhatToMount::Fs(fs), flags).context("mounting fs")?;
        self.mounts.push(current_node);

        Ok(())
    }

    fn unmount(&mut self) -> Result<(), Errno> {
        while let Some(node) = self.mounts.pop() {
            node.unmount(UnmountFlags::DETACH)?;
        }
        Ok(())
    }
}

impl Drop for MountRecord {
    fn drop(&mut self) {
        match self.unmount() {
            Ok(()) => {}
            Err(e) => log_error!("failed to unmount during component exit: {:?}", e),
        }
    }
}
