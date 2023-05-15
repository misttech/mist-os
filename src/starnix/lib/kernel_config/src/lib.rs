// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Error};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_decl as fdecl;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_process as fprocess;
use fuchsia_runtime as fruntime;
use fuchsia_zircon::HandleBased;
use rand::Rng;
use vfs::directory::helper::DirectlyMutable;

/// The handle type that is used to pass configuration handles to the starnix_kernel.
pub const HANDLE_TYPE: fruntime::HandleType = fruntime::HandleType::User0;

/// The handle info that is used to pass the /pkg directory of a container component.
pub const PKG_HANDLE_INFO: fruntime::HandleInfo = fruntime::HandleInfo::new(HANDLE_TYPE, 0);

/// The handle info that is used to pass the outgoing directory for a container.
pub const CONTAINER_OUTGOING_DIR_HANDLE_INFO: fruntime::HandleInfo =
    fruntime::HandleInfo::new(HANDLE_TYPE, 1);

/// The handle info that is used to pass the svc directory for a container.
pub const CONTAINER_SVC_HANDLE_INFO: fruntime::HandleInfo =
    fruntime::HandleInfo::new(HANDLE_TYPE, 2);

#[derive(Debug)]
pub struct KernelStartInfo {
    pub args: fcomponent::CreateChildArgs,
    pub name: String,
}

fn take_namespace_entry(
    ns: &mut Vec<frunner::ComponentNamespaceEntry>,
    entry_name: &str,
) -> Result<Option<ClientEnd<fio::DirectoryMarker>>, Error> {
    if let Some(entry) = ns.iter_mut().find(|entry| entry.path == Some(entry_name.to_string())) {
        if entry.directory.is_none() {
            bail!("Missing directory handle in {entry_name} namespace entry");
        }
        Ok(entry.directory.take())
    } else {
        Ok(None)
    }
}

pub fn generate_kernel_config(
    kernels_dir: &vfs::directory::immutable::Simple,
    kernels_dir_name: &str,
    mut component_start_info: frunner::ComponentStartInfo,
) -> Result<KernelStartInfo, Error> {
    // The name of the directory as seen by the starnix_kernel.
    const CONFIG_DIRECTORY: &str = "container_config";

    // Grab the /pkg directory of the container component, and pass it to the starnix_kernel as handle
    // `User1`.
    let mut ns = component_start_info.ns.take().ok_or_else(|| anyhow!("Missing namespace"))?;
    let pkg = take_namespace_entry(&mut ns, "/pkg")?
        .ok_or_else(|| anyhow!("Missing /pkg entry in namespace"))?;
    let pkg_handle_info = fprocess::HandleInfo {
        handle: pkg.into_channel().into_handle(),
        id: PKG_HANDLE_INFO.as_raw(),
    };

    // Pass the outgoing directory of the container to the starnix_kernel. The kernel uses this to
    // serve, for example, a component runner on behalf of the container.
    let outgoing_dir =
        component_start_info.outgoing_dir.take().expect("Missing outgoing directory.");
    let outgoing_dir_handle_info = fprocess::HandleInfo {
        handle: outgoing_dir.into_handle(),
        id: CONTAINER_OUTGOING_DIR_HANDLE_INFO.as_raw(),
    };

    let mut numbered_handles = vec![pkg_handle_info, outgoing_dir_handle_info];

    if let Some(svc) = take_namespace_entry(&mut ns, "/svc")? {
        // Pass the svc directory of the container to the starnix kernel. The kernel used this to
        // obtain services offered to the container.
        let svc_handle_info = fprocess::HandleInfo {
            handle: svc.into_channel().into_handle(),
            id: CONTAINER_SVC_HANDLE_INFO.as_raw(),
        };
        numbered_handles.push(svc_handle_info);
    }

    // Create a new configuration directory for the starnix_kernel instance.
    let kernel_name = generate_kernel_config_directory(kernels_dir, &mut component_start_info)?;

    let args = fcomponent::CreateChildArgs {
        dynamic_offers: Some(vec![fdecl::Offer::Directory(fdecl::OfferDirectory {
            source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
            source_name: Some(kernels_dir_name.to_string()),
            target_name: Some(CONFIG_DIRECTORY.to_string()),
            rights: Some(
                fio::Operations::READ_BYTES
                    | fio::Operations::CONNECT
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE
                    | fio::Operations::GET_ATTRIBUTES,
            ),
            subdir: Some(kernel_name.clone()),
            dependency_type: Some(fdecl::DependencyType::Strong),
            availability: Some(fdecl::Availability::Required),
            ..Default::default()
        })]),
        numbered_handles: Some(numbered_handles),
        ..Default::default()
    };

    Ok(KernelStartInfo { args, name: kernel_name })
}

/// Takes a `name` and generates a `String` suitable to use as the name of a component in a
/// collection.
///
/// Used to avoid creating children with the same name.
fn generate_kernel_name(name: &str) -> String {
    let random_id: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();
    name.to_owned() + "_" + &random_id
}

/// Creates a new subdirectory in `kernels_dir`, named after the container being created.
///
/// This directory is populated with a file containing the container's `program` block.
///
/// Returns the name of the newly created directory.
fn generate_kernel_config_directory(
    kernels_dir: &vfs::directory::immutable::Simple,
    component_start_info: &mut frunner::ComponentStartInfo,
) -> Result<String, Error> {
    // The name of the file that the starnix_kernel's configuration is written to.
    const CONFIG_FILE: &str = "config";

    let program_block = component_start_info
        .program
        .take()
        .ok_or(anyhow!("Missing program block in container."))?;

    // Add the container configuration file to the directory that is provided to the starnix_kernel.
    let kernel_config_file = vfs::file::vmo::read_only(fidl::persist(&program_block)?);

    let kernel_config_dir = vfs::directory::immutable::simple();
    kernel_config_dir.add_entry(CONFIG_FILE, kernel_config_file)?;

    // This particular starnix_kernel's configuration directory is stored in the `kernels_dir`. We
    // then offer a subdirectory of said directory to the starnix_kernel.
    let kernel_name = {
        let container_url =
            component_start_info.resolved_url.clone().ok_or(anyhow!("Missing resolved URL"))?;
        let container_name = container_url
            .split('/')
            .last()
            .expect("Could not find last path component in resolved URL");
        generate_kernel_name(container_name)
    };

    kernels_dir.add_entry(kernel_name.clone(), kernel_config_dir)?;

    Ok(kernel_name)
}
