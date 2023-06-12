// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::layout;
use crate::trampoline;
use fidl::{
    endpoints::{ClientEnd, Proxy},
    HandleBased,
};
use fidl_fuchsia_dash::LauncherError;
use fidl_fuchsia_hardware_pty as pty;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_process as fproc;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_runtime::{HandleInfo as HandleId, HandleType};
use fuchsia_zircon as zx;

pub mod component;
pub mod package;

// -s: force input from stdin
// -i: force interactive
const DASH_ARGS_FOR_INTERACTIVE: [&[u8]; 2] = ["-i".as_bytes(), "-s".as_bytes()];
// TODO(fxbug.dev/104634): Verbose (-v) or write-commands-to-stderr (-x) is required if a command is
// given, else it errors with `Can't open <cmd>`. -c: execute command
const DASH_ARGS_FOR_COMMAND: [&[u8]; 2] = ["-v".as_bytes(), "-c".as_bytes()];

async fn explore_over_handles(
    stdin: zx::Handle,
    stdout: zx::Handle,
    stderr: zx::Handle,
    mut tool_urls: Vec<String>,
    command: Option<String>,
    mut name_infos: Vec<fproc::NameInfo>,
    process_name: String,
) -> Result<zx::Process, LauncherError> {
    // In addition to tools binaries requested by the user, add the built-in binaries of the
    // debug-dash-launcher package, creating `#!resolve` trampolines for all.
    tool_urls.push("fuchsia-pkg://fuchsia.com/debug-dash-launcher".into());
    let (tools_pkg_dir, tools_path) =
        trampoline::create_trampolines_from_packages(tool_urls).await?;
    layout::add_tools_to_name_infos(tools_pkg_dir, &mut name_infos);

    // The dash-launcher can be asked to launch multiple dash processes, each of which can make
    // their own process hierarchies. This will look better topologically if we make a child job for
    // each dash process.
    let job =
        fuchsia_runtime::job_default().create_child_job().map_err(|_| LauncherError::Internal)?;

    // Add handles for the current job, stdio, library loader and UTC time.
    let handle_infos = create_handle_infos(&job, stdin, stdout, stderr)?;

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
