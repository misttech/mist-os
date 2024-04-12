// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::layout;
use crate::socket;
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_dash as fdash;
use fidl_fuchsia_hardware_pty as pty;
use fuchsia_zircon as zx;

pub async fn explore_over_socket(
    fuchsia_pkg_resolver: fdash::FuchsiaPkgResolver,
    url: &str,
    subpackages: &[String],
    socket: zx::Socket,
    tool_urls: Vec<String>,
    command: Option<String>,
) -> Result<zx::Process, fdash::LauncherError> {
    let pty = socket::spawn_pty_forwarder(socket).await?;
    explore_over_pty(fuchsia_pkg_resolver, url, subpackages, pty, tool_urls, command).await
}

async fn explore_over_pty(
    fuchsia_pkg_resolver: fdash::FuchsiaPkgResolver,
    url: &str,
    subpackages: &[String],
    pty: ClientEnd<pty::DeviceMarker>,
    tool_urls: Vec<String>,
    command: Option<String>,
) -> Result<zx::Process, fdash::LauncherError> {
    let (stdin, stdout, stderr) = super::split_pty_into_handles(pty)?;
    explore_over_handles(
        fuchsia_pkg_resolver,
        url,
        subpackages,
        stdin,
        stdout,
        stderr,
        tool_urls,
        command,
    )
    .await
}

pub async fn explore_over_handles(
    fuchsia_pkg_resolver: fdash::FuchsiaPkgResolver,
    url: &str,
    subpackages: &[String],
    stdin: zx::Handle,
    stdout: zx::Handle,
    stderr: zx::Handle,
    tool_urls: Vec<String>,
    command: Option<String>,
) -> Result<zx::Process, fdash::LauncherError> {
    let package_resolver = crate::package_resolver::PackageResolver::new(fuchsia_pkg_resolver)?;
    let dir = package_resolver.resolve_subpackage(url, subpackages).await?;

    // Add all the necessary entries, except for the tools, into the dash namespace.
    let name_infos =
        layout::package_layout(layout::serve_process_launcher_and_resolver_svc_dir()?, dir);

    // Set a name for the dash process of the package we're exploring that is easy to find. If
    // the url is `fuchsia-pkg://fuchsia.example/update`, the process name is `sh-update`.
    let process_name = if let Ok(url) = fuchsia_url::AbsolutePackageUrl::parse(url) {
        url.name().to_string()
    } else {
        url.replace('/', "-").into()
    };
    let process_name = format!("sh-{process_name}");

    super::explore_over_handles(
        stdin,
        stdout,
        stderr,
        tool_urls,
        command,
        name_infos,
        process_name,
        &crate::package_resolver::PackageResolver::new(fuchsia_pkg_resolver)?,
    )
    .await
}
