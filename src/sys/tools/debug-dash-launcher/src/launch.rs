// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::layout;
use crate::socket;
use crate::trampoline;
use fidl::{
    endpoints::{create_proxy, ClientEnd, Proxy, ServerEnd},
    HandleBased,
};
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_dash as fdash;
use fidl_fuchsia_dash::LauncherError;
use fidl_fuchsia_hardware_pty as pty;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_process as fproc;
use fidl_fuchsia_sys2 as fsys;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_runtime::{HandleInfo as HandleId, HandleType};
use fuchsia_zircon as zx;
use moniker::{RelativeMoniker, RelativeMonikerBase};
use tracing::warn;

// -s: force input from stdin
// -i: force interactive
const DASH_ARGS_FOR_INTERACTIVE: [&[u8]; 2] = ["-i".as_bytes(), "-s".as_bytes()];
// TODO(fxbug.dev/104634): Verbose (-v) or write-commands-to-stderr (-x) is required if a command is
// given, else it errors with `Can't open <cmd>`. -c: execute command
const DASH_ARGS_FOR_COMMAND: [&[u8]; 2] = ["-v".as_bytes(), "-c".as_bytes()];

pub async fn launch_with_socket(
    moniker: &str,
    socket: zx::Socket,
    tool_urls: Vec<String>,
    command: Option<String>,
    ns_layout: fdash::DashNamespaceLayout,
) -> Result<zx::Process, LauncherError> {
    let pty = socket::spawn_pty_forwarder(socket).await?;
    launch_with_pty(moniker, pty, tool_urls, command, ns_layout).await
}

pub async fn launch_with_pty(
    moniker: &str,
    pty: ClientEnd<pty::DeviceMarker>,
    tool_urls: Vec<String>,
    command: Option<String>,
    ns_layout: fdash::DashNamespaceLayout,
) -> Result<zx::Process, LauncherError> {
    let (stdin, stdout, stderr) = split_pty_into_handles(pty)?;
    launch_with_handles(moniker, stdin, stdout, stderr, tool_urls, command, ns_layout).await
}

pub async fn launch_with_handles(
    moniker: &str,
    stdin: zx::Handle,
    stdout: zx::Handle,
    stderr: zx::Handle,
    mut tool_urls: Vec<String>,
    command: Option<String>,
    ns_layout: fdash::DashNamespaceLayout,
) -> Result<zx::Process, LauncherError> {
    // Get all directories needed to launch dash successfully.
    let query =
        connect_to_protocol::<fsys::RealmQueryMarker>().map_err(|_| LauncherError::RealmQuery)?;

    let moniker = RelativeMoniker::parse_str(moniker).map_err(|_| LauncherError::BadMoniker)?;
    let moniker = moniker.to_string();

    let out_dir = open_outgoing_dir(&query, &moniker).await?;
    let runtime_dir = open_runtime_dir(&query, &moniker).await?;
    let exposed_dir = open_exposed_dir(&query, &moniker).await?;
    let ns_entries = construct_namespace(&query, &moniker).await?;

    // In addition to tools binaries requested by the user, add the built-in binaries of the
    // debug-dash-launcher package, creating `#!resolve` trampolines for all.
    tool_urls.push("fuchsia-pkg://fuchsia.com/debug-dash-launcher".to_string());
    let (tools_pkg_dir, tools_path) =
        trampoline::create_trampolines_from_packages(tool_urls).await?;

    // The dash-launcher can be asked to launch multiple dash processes, each of which can make
    // their own process hierarchies. This will look better topologically if we make a child job for
    // each dash process.
    let job =
        fuchsia_runtime::job_default().create_child_job().map_err(|_| LauncherError::Internal)?;

    // Add handles for the current job, stdio, library loader and UTC time.
    let handle_infos = create_handle_infos(&job, stdin, stdout, stderr)?;

    // Add all the necessary entries into the dash namespace.
    let name_infos = match ns_layout {
        fdash::DashNamespaceLayout::NestAllInstanceDirs => {
            // Add a custom `/svc` directory to dash that contains `fuchsia.process.Launcher` and
            // `fuchsia.process.Resolver`.
            let svc_dir = layout::serve_process_launcher_and_resolver_svc_dir()?;

            layout::nest_all_instance_dirs(
                ns_entries,
                exposed_dir,
                svc_dir,
                out_dir,
                runtime_dir,
                tools_pkg_dir,
            )
        }
        fdash::DashNamespaceLayout::InstanceNamespaceIsRoot => {
            layout::instance_namespace_is_root(ns_entries, tools_pkg_dir).await
        }
    };

    // Set a name for the dash process of the component we're exploring that is easy to find. If
    // moniker is `./core/foo`, process name is `sh-core-foo`.
    let mut process_name = moniker.replace('/', "-");
    process_name.remove(0);
    let process_name = format!("sh{}", process_name);

    let launcher = connect_to_protocol::<fproc::LauncherMarker>()
        .map_err(|_| LauncherError::ProcessLauncher)?;

    let mut args = Vec::new();
    if let Some(cmd) = command {
        args.extend(DASH_ARGS_FOR_COMMAND.iter().map(|b| b.to_vec()));
        args.push(cmd.into_bytes());
    } else {
        args.extend(DASH_ARGS_FOR_INTERACTIVE.iter().map(|b| b.to_vec()));
    };

    // Spawn the dash process.
    let info = create_launch_info(process_name, &job).await?;
    launcher.add_names(name_infos).map_err(|_| LauncherError::ProcessLauncher)?;
    launcher.add_handles(handle_infos).map_err(|_| LauncherError::ProcessLauncher)?;
    launcher.add_args(&args).map_err(|_| LauncherError::ProcessLauncher)?;
    let path_envvar = trampoline::create_env_path(tools_path);
    let env_vars = &[path_envvar.into_bytes()];
    launcher.add_environs(env_vars).map_err(|_| LauncherError::ProcessLauncher)?;
    let (status, process) =
        launcher.launch(info).await.map_err(|_| LauncherError::ProcessLauncher)?;
    zx::Status::ok(status).map_err(|_| LauncherError::ProcessLauncher)?;
    let process = process.ok_or(LauncherError::ProcessLauncher)?;

    // The job should be terminated when the dash process dies.
    job.set_critical(zx::JobCriticalOptions::empty(), &process)
        .map_err(|_| LauncherError::Internal)?;

    Ok(process)
}

fn split_pty_into_handles(
    pty: ClientEnd<pty::DeviceMarker>,
) -> Result<(zx::Handle, zx::Handle, zx::Handle), LauncherError> {
    let pty = pty.into_proxy().unwrap();

    // Split the PTY into 3 channels (stdin, stdout, stderr).
    let (stdout, to_pty_stdout) = fidl::endpoints::create_endpoints::<pty::DeviceMarker>();
    let (stderr, to_pty_stderr) = fidl::endpoints::create_endpoints::<pty::DeviceMarker>();
    let to_pty_stdout = to_pty_stdout.into_channel().into();
    let to_pty_stderr = to_pty_stderr.into_channel().into();

    // Clone the PTY to also be used for stdout and stderr.
    pty.clone2(to_pty_stdout).map_err(|_| LauncherError::Pty)?;
    pty.clone2(to_pty_stderr).map_err(|_| LauncherError::Pty)?;

    let stdin = pty.into_channel().unwrap().into_zx_channel().into_handle();
    let stdout = stdout.into_handle();
    let stderr = stderr.into_handle();

    Ok((stdin, stdout, stderr))
}

async fn open_outgoing_dir(
    query: &fsys::RealmQueryProxy,
    moniker: &str,
) -> Result<Option<fio::DirectoryProxy>, LauncherError> {
    let (dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let server_end = ServerEnd::new(server_end.into_channel());
    let result = query
        .open(
            &moniker,
            fsys::OpenDirType::OutgoingDir,
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            ".",
            server_end,
        )
        .await
        .map_err(|error| {
            warn!(%moniker, %error, "FIDL call failed to open outgoing dir");
            LauncherError::RealmQuery
        })?;
    match result {
        Ok(()) => Ok(Some(dir)),
        Err(fsys::OpenError::InstanceNotRunning) | Err(fsys::OpenError::NoSuchDir) => Ok(None),
        Err(fsys::OpenError::BadMoniker) => Err(LauncherError::BadMoniker),
        Err(fsys::OpenError::InstanceNotResolved) => Err(LauncherError::InstanceNotResolved),
        Err(fsys::OpenError::InstanceNotFound) => Err(LauncherError::InstanceNotFound),
        Err(error) => {
            warn!(%moniker, ?error, "RealmQuery returned error opening outgoing dir");
            Err(LauncherError::RealmQuery)
        }
    }
}

async fn open_runtime_dir(
    query: &fsys::RealmQueryProxy,
    moniker: &str,
) -> Result<Option<fio::DirectoryProxy>, LauncherError> {
    let (dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let server_end = ServerEnd::new(server_end.into_channel());
    let result = query
        .open(
            &moniker,
            fsys::OpenDirType::RuntimeDir,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            ".",
            server_end,
        )
        .await
        .map_err(|error| {
            warn!(%moniker, %error, "FIDL call failed to open runtime dir");
            LauncherError::RealmQuery
        })?;
    match result {
        Ok(()) => Ok(Some(dir)),
        Err(fsys::OpenError::InstanceNotRunning) | Err(fsys::OpenError::NoSuchDir) => Ok(None),
        Err(fsys::OpenError::BadMoniker) => Err(LauncherError::BadMoniker),
        Err(fsys::OpenError::InstanceNotResolved) => Err(LauncherError::InstanceNotResolved),
        Err(fsys::OpenError::InstanceNotFound) => Err(LauncherError::InstanceNotFound),
        Err(error) => {
            warn!(%moniker, ?error, "RealmQuery returned error opening runtime dir");
            Err(LauncherError::RealmQuery)
        }
    }
}

async fn open_exposed_dir(
    query: &fsys::RealmQueryProxy,
    moniker: &str,
) -> Result<fio::DirectoryProxy, LauncherError> {
    let (dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let server_end = ServerEnd::new(server_end.into_channel());
    let result = query
        .open(
            &moniker,
            fsys::OpenDirType::ExposedDir,
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            ".",
            server_end,
        )
        .await
        .map_err(|error| {
            warn!(%moniker, %error, "FIDL call failed to open exposed dir");
            LauncherError::RealmQuery
        })?;
    match result {
        Ok(()) => Ok(dir),
        Err(fsys::OpenError::BadMoniker) => Err(LauncherError::BadMoniker),
        Err(fsys::OpenError::InstanceNotResolved) => Err(LauncherError::InstanceNotResolved),
        Err(fsys::OpenError::InstanceNotFound) => Err(LauncherError::InstanceNotFound),
        Err(error) => {
            warn!(%moniker, ?error, "RealmQuery returned error opening exposed dir");
            Err(LauncherError::RealmQuery)
        }
    }
}

async fn construct_namespace(
    query: &fsys::RealmQueryProxy,
    relative_moniker: &str,
) -> Result<Vec<fcrunner::ComponentNamespaceEntry>, LauncherError> {
    let result = query
        .construct_namespace(&relative_moniker)
        .await
        .map_err(|_| LauncherError::RealmQuery)?;
    match result {
        Ok(ns_entries) => Ok(ns_entries),
        Err(fsys::ConstructNamespaceError::BadMoniker) => Err(LauncherError::BadMoniker),
        Err(fsys::ConstructNamespaceError::InstanceNotResolved) => {
            Err(LauncherError::InstanceNotResolved)
        }
        Err(fsys::ConstructNamespaceError::InstanceNotFound) => {
            Err(LauncherError::InstanceNotFound)
        }
        Err(_) => Err(LauncherError::RealmQuery),
    }
}

fn create_handle_infos(
    job: &zx::Job,
    stdin: zx::Handle,
    stdout: zx::Handle,
    stderr: zx::Handle,
) -> Result<Vec<fproc::HandleInfo>, LauncherError> {
    let stdin_handle = fproc::HandleInfo {
        handle: stdin.into_handle(),
        id: HandleId::new(HandleType::FileDescriptor, 0).as_raw(),
    };

    let stdout_handle = fproc::HandleInfo {
        handle: stdout.into_handle(),
        id: HandleId::new(HandleType::FileDescriptor, 1).as_raw(),
    };

    let stderr_handle = fproc::HandleInfo {
        handle: stderr.into_handle(),
        id: HandleId::new(HandleType::FileDescriptor, 2).as_raw(),
    };

    let job_dup =
        job.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(|_| LauncherError::Internal)?;
    let job_handle = fproc::HandleInfo {
        handle: zx::Handle::from(job_dup),
        id: HandleId::new(HandleType::DefaultJob, 0).as_raw(),
    };

    let ldsvc = fuchsia_runtime::loader_svc().map_err(|_| LauncherError::Internal)?;
    let ldsvc_handle =
        fproc::HandleInfo { handle: ldsvc, id: HandleId::new(HandleType::LdsvcLoader, 0).as_raw() };

    let utc_clock = {
        let utc_clock = fuchsia_runtime::duplicate_utc_clock_handle(zx::Rights::SAME_RIGHTS)
            .map_err(|_| LauncherError::Internal)?;
        utc_clock.into_handle()
    };
    let utc_clock_handle = fproc::HandleInfo {
        handle: utc_clock,
        id: HandleId::new(HandleType::ClockUtc, 0).as_raw(),
    };

    Ok(vec![stdin_handle, stdout_handle, stderr_handle, job_handle, ldsvc_handle, utc_clock_handle])
}

async fn create_launch_info(
    process_name: String,
    job: &zx::Job,
) -> Result<fproc::LaunchInfo, LauncherError> {
    // Load `/pkg/bin/sh` as an executable VMO and pass it to the Launcher.
    let dash_file = fuchsia_fs::file::open_in_namespace(
        "/pkg/bin/sh",
        fio::OpenFlags::RIGHT_EXECUTABLE | fio::OpenFlags::RIGHT_READABLE,
    )
    .map_err(|_| LauncherError::DashBinary)?;

    let executable = dash_file
        .get_backing_memory(
            fio::VmoFlags::READ | fio::VmoFlags::EXECUTE | fio::VmoFlags::PRIVATE_CLONE,
        )
        .await
        .map_err(|_| LauncherError::DashBinary)?
        .map_err(|_| LauncherError::DashBinary)?;

    let job_dup =
        job.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(|_| LauncherError::Internal)?;

    let name = truncate_str(&process_name, zx::sys::ZX_MAX_NAME_LEN).to_owned();

    Ok(fproc::LaunchInfo { name, job: job_dup, executable })
}

/// Truncates `s` to be at most `max_len` bytes.
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    // TODO(https://github.com/rust-lang/rust/issues/93743): Use floor_char_boundary when stable.
    let mut index = max_len;
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    &s[..index]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_async as fasync;
    use futures::StreamExt;

    fn serve_realm_query(mut result: fsys::RealmQueryOpenResult) -> fsys::RealmQueryProxy {
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<fsys::RealmQueryMarker>().unwrap();
        fasync::Task::spawn(async move {
            let mut stream = server_end.into_stream().unwrap();
            let request = stream.next().await.unwrap().unwrap();
            let (moniker, responder) = match request {
                fsys::RealmQueryRequest::Open { moniker, responder, .. } => (moniker, responder),
                _ => panic!("Unexpected RealmQueryRequest"),
            };
            assert_eq!(moniker, ".");
            responder.send(&mut result).unwrap();
        })
        .detach();
        proxy
    }

    #[fuchsia::test]
    async fn open_success() {
        let query = serve_realm_query(Ok(()));
        open_exposed_dir(&query, ".").await.unwrap();
    }

    #[fuchsia::test]
    async fn instance_not_running() {
        let query = serve_realm_query(Err(fsys::OpenError::InstanceNotRunning));
        assert!(open_outgoing_dir(&query, ".").await.unwrap().is_none());
    }

    #[fuchsia::test]
    async fn no_such_dir() {
        let query = serve_realm_query(Err(fsys::OpenError::NoSuchDir));
        assert!(open_runtime_dir(&query, ".").await.unwrap().is_none());
    }

    #[fuchsia::test]
    async fn error_instance_unresolved() {
        let query = serve_realm_query(Err(fsys::OpenError::InstanceNotResolved));
        let error = open_exposed_dir(&query, ".").await.unwrap_err();
        assert_eq!(error, LauncherError::InstanceNotResolved);
    }

    #[fuchsia::test]
    async fn error_instance_not_found() {
        let query = serve_realm_query(Err(fsys::OpenError::InstanceNotFound));
        let error = open_exposed_dir(&query, ".").await.unwrap_err();
        assert_eq!(error, LauncherError::InstanceNotFound);
    }
}
