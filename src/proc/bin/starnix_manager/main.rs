// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, format_err, Context, Error};
use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_decl as fdecl;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_process as fprocess;
use fidl_fuchsia_starnix_developer as fstardev;
use fidl_fuchsia_starnix_galaxy as fstargalaxy;
use frunner::ComponentStartInfo;
use fuchsia_component::client::{self as fclient, connect_to_protocol};
use fuchsia_runtime::{HandleInfo, HandleType};
use fuchsia_zircon as zx;
use fuchsia_zircon::HandleBased;
use futures::TryStreamExt;
use rand::Rng;
use std::sync::Arc;
use vfs::directory::{entry::DirectoryEntry, helper::DirectlyMutable};

#[fuchsia::main(logging_tags = ["starnix_manager"])]
async fn main() -> Result<(), Error> {
    const KERNELS_DIRECTORY: &str = "kernels";
    const SVC_DIRECTORY: &str = "svc";

    let outgoing_dir_handle =
        fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleType::DirectoryRequest.into())
            .ok_or(anyhow!("Failed to get startup handle"))?;
    let outgoing_dir_server_end =
        fidl::endpoints::ServerEnd::new(zx::Channel::from(outgoing_dir_handle));

    let outgoing_dir = vfs::directory::immutable::simple();
    let kernels_dir = vfs::directory::immutable::simple();
    outgoing_dir.add_entry(KERNELS_DIRECTORY, kernels_dir.clone())?;

    let svc_dir = vfs::directory::immutable::simple();
    svc_dir.add_entry(
        fstardev::ManagerMarker::PROTOCOL_NAME,
        vfs::service::host(move |requests| async move {
            serve_starnix_manager(requests).await.expect("Error serving starnix manager.");
        }),
    )?;
    svc_dir.add_entry(
        frunner::ComponentRunnerMarker::PROTOCOL_NAME,
        vfs::service::host(move |requests| {
            let kernels_dir = kernels_dir.clone();
            async move {
                serve_component_runner(requests, kernels_dir.clone())
                    .await
                    .expect("Error serving component runner.");
            }
        }),
    )?;
    outgoing_dir.add_entry(SVC_DIRECTORY, svc_dir.clone())?;

    let execution_scope = vfs::execution_scope::ExecutionScope::new();
    outgoing_dir.open(
        execution_scope.clone(),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        0,
        vfs::path::Path::dot(),
        outgoing_dir_server_end,
    );
    execution_scope.wait().await;

    Ok(())
}

pub async fn serve_starnix_manager(
    mut request_stream: fstardev::ManagerRequestStream,
) -> Result<(), Error> {
    while let Some(event) = request_stream.try_next().await? {
        match event {
            fstardev::ManagerRequest::Start { url, responder } => {
                let args = fcomponent::CreateChildArgs {
                    numbered_handles: None,
                    ..fcomponent::CreateChildArgs::EMPTY
                };
                if let Err(e) = create_child_component(url, args).await {
                    tracing::error!("failed to create child component: {}", e);
                }
                responder.send()?;
            }
            fstardev::ManagerRequest::StartShell { params, controller, .. } => {
                start_shell(params, controller).await?;
            }
            fstardev::ManagerRequest::VsockConnect { galaxy, port, bridge_socket, .. } => {
                connect_to_vsock(port, bridge_socket, &galaxy).unwrap_or_else(|e| {
                    tracing::error!("failed to connect to vsock {:?}", e);
                });
            }
        }
    }
    Ok(())
}

pub async fn serve_component_runner(
    mut request_stream: frunner::ComponentRunnerRequestStream,
    kernels_dir: Arc<vfs::directory::immutable::Simple>,
) -> Result<(), Error> {
    while let Some(event) = request_stream.try_next().await? {
        match event {
            frunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                create_new_kernel(&kernels_dir, start_info, controller).await?;
            }
        }
    }
    Ok(())
}

async fn start_shell(
    params: fstardev::ShellParams,
    controller: ServerEnd<fstardev::ShellControllerMarker>,
) -> Result<(), Error> {
    let controller_handle_info = fprocess::HandleInfo {
        handle: controller.into_channel().into_handle(),
        id: HandleInfo::new(HandleType::User0, 0).as_raw(),
    };
    let numbered_handles = vec![
        handle_info_from_socket(params.standard_in, 0)?,
        handle_info_from_socket(params.standard_out, 1)?,
        handle_info_from_socket(params.standard_err, 2)?,
        controller_handle_info,
    ];
    let args = fcomponent::CreateChildArgs {
        numbered_handles: Some(numbered_handles),
        ..fcomponent::CreateChildArgs::EMPTY
    };

    let url = params.url.ok_or(anyhow!("No shell URL specified"))?;
    create_child_component(url, args).await
}

/// Connects `bridge_socket` to the vsocket at `port` in the specified galaxy.
///
/// Returns an error if the FIDL connection to the galaxy failed.
fn connect_to_vsock(port: u32, bridge_socket: fidl::Socket, galaxy: &str) -> Result<(), Error> {
    let service_prefix = "/".to_string() + galaxy;
    let galaxy = fclient::connect_to_protocol_at::<fstargalaxy::ControllerMarker>(&service_prefix)?;

    galaxy.vsock_connect(port, bridge_socket).context("Failed to call vsock connect on galaxy")
}

/// Creates a `HandleInfo` from the provided socket and file descriptor.
///
/// The file descriptor is encoded as a `PA_HND(PA_FD, <file_descriptor>)` before being stored in
/// the `HandleInfo`.
///
/// Returns an error if `socket` is `None`.
pub fn handle_info_from_socket(
    socket: Option<fidl::Socket>,
    file_descriptor: u16,
) -> Result<fprocess::HandleInfo, Error> {
    if let Some(socket) = socket {
        let info = HandleInfo::new(HandleType::FileDescriptor, file_descriptor);
        Ok(fprocess::HandleInfo { handle: socket.into_handle(), id: info.as_raw() })
    } else {
        Err(anyhow!("Failed to create HandleInfo for {}", file_descriptor))
    }
}

/// Creates a new child component in the `playground` collection.
///
/// # Parameters
/// - `url`: The URL of the component to create.
/// - `args`: The `CreateChildArgs` that are passed to the component manager.
pub async fn create_child_component(
    url: String,
    args: fcomponent::CreateChildArgs,
) -> Result<(), Error> {
    // TODO(fxbug.dev/74511): The amount of setup required here is a bit lengthy. Ideally,
    // fuchsia-component would provide language-specific bindings for the Realm API that could
    // reduce this logic to a few lines.

    const COLLECTION: &str = "playground";
    let realm = fclient::realm().context("failed to connect to Realm service")?;
    let mut collection_ref = fdecl::CollectionRef { name: COLLECTION.into() };
    let id: u64 = rand::thread_rng().gen();
    let child_name = format!("starnix-{}", id);
    let child_decl = fdecl::Child {
        name: Some(child_name.clone()),
        url: Some(url),
        startup: Some(fdecl::StartupMode::Lazy),
        environment: None,
        ..fdecl::Child::EMPTY
    };
    let () = realm
        .create_child(&mut collection_ref, child_decl, args)
        .await?
        .map_err(|e| format_err!("failed to create child: {:?}", e))?;
    // The component is run in a `SingleRun` collection instance, and will be automatically
    // deleted when it exits.
    Ok(())
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

/// Creates a new instance of `starnix_kernel`.
///
/// This is done by creating a new child in the `kernels` collection. A directory is offered to the
/// `starnix_kernel`, at `/galaxy_config`. The directory contains the `program` block of the galaxy,
/// and the galaxy's `/pkg` directory.
///
/// The component controller for the galaxy is also passed to the `starnix_kernel`, via the `User0`
/// handle. The `controller` is closed when the `starnix_kernel` finishes executing (e.g., when
/// the init task completes).
async fn create_new_kernel(
    kernels_dir: &vfs::directory::immutable::Simple,
    mut component_start_info: frunner::ComponentStartInfo,
    _controller: ServerEnd<frunner::ComponentControllerMarker>,
) -> Result<(), Error> {
    // The name of the collection that the starnix_kernel is run in.
    const KERNEL_COLLECTION: &str = "kernels";
    // The name of the directory capability that is being offered to the starnix_kernel.
    const KERNEL_DIRECTORY: &str = "kernels";
    // The url of the starnix_kernel component, which is packaged with the starnix_manager.
    const KERNEL_URL: &str = "starnix_kernel#meta/starnix_kernel.cm";
    // The name of the directory as seen by the starnix_kernel.
    const CONFIG_DIRECTORY: &str = "galaxy_config";

    // Grab the /pkg directory of the galaxy component, and pass it to the starnix_kernel as handle
    // `User1`.
    let mut ns = component_start_info.ns.take().ok_or_else(|| anyhow!("Missing namespace"))?;
    let pkg = ns
        .iter_mut()
        .find(|entry| entry.path == Some("/pkg".to_string()))
        .ok_or_else(|| anyhow!("Missing /pkg entry in namespace"))?
        .directory
        .take()
        .ok_or_else(|| anyhow!("Missing directory handle in pkg namespace entry"))?;
    let pkg_handle_info = fprocess::HandleInfo {
        handle: pkg.into_channel().into_handle(),
        id: kernel_config::PKG_HANDLE_INFO.as_raw(),
    };

    // Pass the outgoing directory of the galaxy to the starnix_kernel. The kernel uses this to
    // serve, for example, a component runner on behalf of the galaxy.
    let outgoing_dir =
        component_start_info.outgoing_dir.take().expect("Missing outgoing directory.");
    let outgoing_dir_handle_info = fprocess::HandleInfo {
        handle: outgoing_dir.into_handle(),
        id: kernel_config::GALAXY_OUTGOING_DIR_HANDLE_INFO.as_raw(),
    };
    let numbered_handles = vec![pkg_handle_info, outgoing_dir_handle_info];

    // Create a new configuration directory for the starnix_kernel instance.
    let kernel_name = generate_kernel_config_directory(kernels_dir, component_start_info)?;

    // Create a new instance of starnix_kernel in the kernel collection. Offer the directory that
    // contains all the configuration information for the galaxy that it is running.
    let realm =
        connect_to_protocol::<fcomponent::RealmMarker>().expect("Failed to connect to realm.");
    realm
        .create_child(
            &mut fdecl::CollectionRef { name: KERNEL_COLLECTION.into() },
            fdecl::Child {
                name: Some(kernel_name.to_string()),
                url: Some(KERNEL_URL.to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                ..fdecl::Child::EMPTY
            },
            fcomponent::CreateChildArgs {
                dynamic_offers: Some(vec![fdecl::Offer::Directory(fdecl::OfferDirectory {
                    source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
                    source_name: Some(KERNEL_DIRECTORY.to_string()),
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
                    ..fdecl::OfferDirectory::EMPTY
                })]),
                numbered_handles: Some(numbered_handles),
                ..fcomponent::CreateChildArgs::EMPTY
            },
        )
        .await?
        .map_err(|e| anyhow::anyhow!("failed to create runner child: {:?}", e))?;

    Ok(())
}

/// Creates a new subdirectory in `kernels_dir`, named after the galaxy being created.
///
/// This directory is populated with a file containing the galaxy's `program` block.
///
/// Returns the name of the newly created directory.
fn generate_kernel_config_directory(
    kernels_dir: &vfs::directory::immutable::Simple,
    component_start_info: ComponentStartInfo,
) -> Result<String, Error> {
    // The name of the file that the starnix_kernel's configuration is written to.
    const CONFIG_FILE: &str = "config";

    let mut program_block =
        component_start_info.program.ok_or(anyhow!("Missing program block in galaxy."))?;

    // Add the galaxy configuration file to the directory that is provided to the starnix_kernel.
    let kernel_config_file =
        vfs::file::vmo::read_only_static(fidl::encoding::persist(&mut program_block)?);

    let kernel_config_dir = vfs::directory::immutable::simple();
    kernel_config_dir.add_entry(CONFIG_FILE, kernel_config_file)?;

    // This particular starnix_kernel's configuration directory is stored in the `kernels_dir`. We
    // then offer a subdirectory of said directory to the starnix_kernel.
    let kernel_name = {
        let galaxy_url =
            component_start_info.resolved_url.clone().ok_or(anyhow!("Missing resolved URL"))?;
        let galaxy_name = galaxy_url
            .split('/')
            .last()
            .expect("Could not find last path component in resolved URL");
        generate_kernel_name(galaxy_name)
    };

    kernels_dir.add_entry(kernel_name.clone(), kernel_config_dir)?;

    Ok(kernel_name)
}
