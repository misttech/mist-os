// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::DiscoverableProtocolMarker;
use fidl::endpoints::Proxy;
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_dash::LauncherError;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_process as fproc;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_channel_to_protocol;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use tracing::warn;
use vfs::{
    directory::{entry::DirectoryEntry, helper::DirectlyMutable},
    execution_scope::ExecutionScope,
    path::Path,
    service::endpoint,
};

pub fn package_layout(
    svc_dir: fio::DirectoryProxy,
    target_dir: fio::DirectoryProxy,
) -> Vec<fproc::NameInfo> {
    vec![to_name_info("/svc", svc_dir), to_name_info("/pkg", target_dir)]
}

/// Returns directory handles + paths for a nested layout using the given directories.
/// In this layout, all instance directories are nested as subdirectories of the root.
/// e.g - namespace is under `/ns`, outgoing directory is under `/out`, etc.
pub fn nest_all_instance_dirs(
    ns_entries: Vec<fcrunner::ComponentNamespaceEntry>,
    exposed_dir: fio::DirectoryProxy,
    svc_dir: fio::DirectoryProxy,
    out_dir: Option<fio::DirectoryProxy>,
    runtime_dir: Option<fio::DirectoryProxy>,
) -> Vec<fproc::NameInfo> {
    let mut name_infos = vec![];

    name_infos.push(to_name_info("/exposed", exposed_dir));
    name_infos.push(to_name_info("/svc", svc_dir));

    for entry in ns_entries {
        let path = format!("/ns{}", entry.path.unwrap());
        let directory = entry.directory.unwrap();
        name_infos.push(fproc::NameInfo { path, directory });
    }

    if let Some(dir) = out_dir {
        name_infos.push(to_name_info("/out", dir));
    }

    if let Some(dir) = runtime_dir {
        name_infos.push(to_name_info("/runtime", dir));
    }

    name_infos
}

pub fn add_tools_to_name_infos(
    tools: Option<fio::DirectoryProxy>,
    name_infos: &mut Vec<fproc::NameInfo>,
) {
    if let Some(dir) = tools {
        name_infos.push(to_name_info("/.dash/tools", dir));
    }
}

/// Returns directory handles + paths for a namespace layout using the given directories.
/// In this layout, the instance namespace is the root. This is a layout that works
/// well for tools and closely resembles what the component instance would see if it queried its
/// own namespace.
///
/// To make the shell work correctly, we need to inject the following into the layout:
/// * fuchsia.process.Resolver and fuchsia.process.Launcher into `/svc`
/// * tools packages, if given, into `/.dash/tools`
///
/// Also returns the corresponding PATH envvar that must be set for the dash shell.
pub async fn instance_namespace_is_root(
    ns_entries: Vec<fcrunner::ComponentNamespaceEntry>,
) -> Vec<fproc::NameInfo> {
    let mut name_infos = vec![];

    for entry in ns_entries {
        let path = entry.path.unwrap();
        let directory = entry.directory.unwrap();

        if path == "/svc" {
            let svc_dir = directory.into_proxy().unwrap();
            let svc_dir = inject_process_launcher_and_resolver(svc_dir).await;
            name_infos.push(to_name_info(&path, svc_dir));
        } else {
            name_infos.push(fproc::NameInfo { path, directory });
        }
    }

    name_infos
}

/// Serves a VFS that contains `fuchsia.process.Launcher` and `fuchsia.process.Resolver`. This is
/// used by the Nested filesystem layout.
pub fn serve_process_launcher_and_resolver_svc_dir() -> Result<fio::DirectoryProxy, LauncherError> {
    // Serve a directory that only provides fuchsia.process.Launcher to dash.
    let (svc_dir, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
        .map_err(|_| LauncherError::Internal)?;

    let mut fs = ServiceFs::new();
    fs.add_proxy_service::<fproc::LauncherMarker, ()>();
    fs.add_proxy_service::<fproc::ResolverMarker, ()>();
    fs.serve_connection(server_end).map_err(|_| LauncherError::ProcessResolver)?;

    fasync::Task::spawn(async move {
        fs.collect::<()>().await;
    })
    .detach();

    Ok(svc_dir)
}

/// Gets the list of all protocols available in the given svc directory and serves a new VFS
/// that injects `fuchsia.process.Launcher` from the launcher namespace and forwards the calls
/// for the other protocols. Also adds the `fuchsia.process.Resolver` for tool trampolines.
async fn inject_process_launcher_and_resolver(svc_dir: fio::DirectoryProxy) -> fio::DirectoryProxy {
    let vfs = vfs::directory::immutable::simple();
    let entries = fuchsia_fs::directory::readdir(&svc_dir).await.unwrap();

    // Create forwarding entries for namespace protocols
    for entry in entries {
        let svc_dir = std::clone::Clone::clone(&svc_dir);
        let protocol_name = entry.name.clone();
        vfs.add_entry(
            protocol_name.clone(),
            endpoint(move |_, channel| {
                let server_end = channel.into_zx_channel().into();
                svc_dir
                    .open(
                        fio::OpenFlags::NOT_DIRECTORY,
                        fio::ModeType::empty(),
                        &protocol_name,
                        server_end,
                    )
                    .unwrap();
            }),
        )
        .unwrap();
    }

    // Add process launcher.
    if let Err(err) = vfs.add_entry(
        fproc::LauncherMarker::PROTOCOL_NAME,
        endpoint(|_, channel| {
            connect_channel_to_protocol::<fproc::LauncherMarker>(channel.into_zx_channel())
                .unwrap();
        }),
    ) {
        warn!(?err, "Could not inject fuchsia.process.Launcher into filesystem layout. Ignoring.");
    }

    // Add process resolver.
    if let Err(err) = vfs.add_entry(
        fproc::ResolverMarker::PROTOCOL_NAME,
        endpoint(|_, channel| {
            connect_channel_to_protocol::<fproc::ResolverMarker>(channel.into_zx_channel())
                .unwrap();
        }),
    ) {
        warn!(?err, "Could not inject fuchsia.process.Resolver into filesystem layout. Ignoring.");
    }
    let (svc_dir, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let server_end = server_end.into_channel().into();
    vfs.open(
        ExecutionScope::new(),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        Path::dot(),
        server_end,
    );

    svc_dir
}

fn to_name_info(path: &str, directory: fio::DirectoryProxy) -> fproc::NameInfo {
    let directory = directory.into_channel().unwrap().into_zx_channel().into();
    fproc::NameInfo { path: path.to_string(), directory }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_temp_dir(file_name: &str) -> fio::DirectoryProxy {
        // Create a temp directory and put a file with name `file_name` inside it.
        let temp_dir = TempDir::new().unwrap();
        let temp_dir_path = temp_dir.into_path();
        let file_path = temp_dir_path.join(file_name);
        std::fs::write(&file_path, "Hippos Rule!").unwrap();
        let temp_dir_path = temp_dir_path.display().to_string();
        fuchsia_fs::directory::open_in_namespace(&temp_dir_path, fio::OpenFlags::RIGHT_READABLE)
            .unwrap()
    }

    fn create_ns_entries() -> Vec<fcrunner::ComponentNamespaceEntry> {
        let ns_subdir = create_temp_dir("ns_subdir");
        let ns_subdir = ns_subdir.into_channel().unwrap().into_zx_channel().into();
        vec![fcrunner::ComponentNamespaceEntry {
            path: Some("/ns_subdir".to_string()),
            directory: Some(ns_subdir),
            ..Default::default()
        }]
    }

    #[fuchsia::test]
    async fn nest_all_instance_dirs_started_with_tools() {
        let exposed_dir = create_temp_dir("exposed");
        let out_dir = create_temp_dir("out");
        let svc_dir = create_temp_dir("svc");
        let runtime_dir = create_temp_dir("runtime");
        let ns_entries = create_ns_entries();

        let ns = nest_all_instance_dirs(
            ns_entries,
            exposed_dir,
            svc_dir,
            Some(out_dir),
            Some(runtime_dir),
        );
        assert_eq!(ns.len(), 5);

        let mut paths: Vec<String> = ns.iter().map(|n| n.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec!["/exposed", "/ns/ns_subdir", "/out", "/runtime", "/svc"]);

        // Make sure that the correct directories were mapped to the correct paths.
        for entry in ns {
            let dir = entry.directory.into_proxy().unwrap();
            let entries = fuchsia_fs::directory::readdir(&dir).await.unwrap();

            // These directories must contain a file with the same name
            let path = PathBuf::from(entry.path);
            let expected_file_name = path.file_name().unwrap().to_str().unwrap().to_string();
            assert_eq!(
                entries,
                vec![fuchsia_fs::directory::DirEntry {
                    name: expected_file_name,
                    kind: fuchsia_fs::directory::DirentKind::File
                }]
            );
        }
    }

    #[fuchsia::test]
    async fn nest_all_instance_dirs_started() {
        let exposed_dir = create_temp_dir("exposed");
        let out_dir = create_temp_dir("out");
        let svc_dir = create_temp_dir("svc");
        let runtime_dir = create_temp_dir("runtime");
        let ns_entries = create_ns_entries();

        let ns = nest_all_instance_dirs(
            ns_entries,
            exposed_dir,
            svc_dir,
            Some(out_dir),
            Some(runtime_dir),
        );
        assert_eq!(ns.len(), 5);

        let mut paths: Vec<String> = ns.iter().map(|n| n.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec!["/exposed", "/ns/ns_subdir", "/out", "/runtime", "/svc"]);

        // Make sure that the correct directories were mapped to the correct paths.
        for entry in ns {
            let dir = entry.directory.into_proxy().unwrap();
            let entries = fuchsia_fs::directory::readdir(&dir).await.unwrap();

            // These directories must contain a file with the same name
            let path = PathBuf::from(entry.path);
            let expected_file_name = path.file_name().unwrap().to_str().unwrap().to_string();
            assert_eq!(
                entries,
                vec![fuchsia_fs::directory::DirEntry {
                    name: expected_file_name,
                    kind: fuchsia_fs::directory::DirentKind::File
                }]
            );
        }
    }

    #[fuchsia::test]
    async fn nest_all_instance_dirs_resolved() {
        let exposed_dir = create_temp_dir("exposed");
        let svc_dir = create_temp_dir("svc");
        let ns_entries = create_ns_entries();

        let ns = nest_all_instance_dirs(ns_entries, exposed_dir, svc_dir, None, None);
        assert_eq!(ns.len(), 3);

        let mut paths: Vec<String> = ns.iter().map(|n| n.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec!["/exposed", "/ns/ns_subdir", "/svc"]);

        // Make sure that the correct directories were mapped to the correct paths.
        for entry in ns {
            let dir = entry.directory.into_proxy().unwrap();
            let entries = fuchsia_fs::directory::readdir(&dir).await.unwrap();

            // These directories must contain a file with the same name
            let path = PathBuf::from(entry.path);
            let expected_file_name = path.file_name().unwrap().to_str().unwrap().to_string();
            assert_eq!(
                entries,
                vec![fuchsia_fs::directory::DirEntry {
                    name: expected_file_name,
                    kind: fuchsia_fs::directory::DirentKind::File
                }]
            );
        }
    }

    #[fuchsia::test]
    async fn instance_namespace_is_root_resolved() {
        let ns_entries = create_ns_entries();

        let ns = instance_namespace_is_root(ns_entries).await;

        let mut paths: Vec<String> = ns.iter().map(|n| n.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec!["/ns_subdir"]);

        // Make sure that the correct directories were mapped to the correct paths.
        for entry in ns {
            let dir = entry.directory.into_proxy().unwrap();
            let entries = fuchsia_fs::directory::readdir(&dir).await.unwrap();

            // These directories must contain a file with the same name
            let path = PathBuf::from(entry.path);
            let expected_file_name = path.file_name().unwrap().to_str().unwrap().to_string();
            assert_eq!(
                entries,
                vec![fuchsia_fs::directory::DirEntry {
                    name: expected_file_name,
                    kind: fuchsia_fs::directory::DirentKind::File
                }]
            );
        }
    }
}
