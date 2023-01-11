// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Error},
    fidl::endpoints::create_proxy,
    fidl::HandleBased,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio,
    fidl_fuchsia_process as fprocess,
    fidl_fuchsia_test::{self as ftest},
    fuchsia_component::client as fclient,
    fuchsia_runtime as fruntime, fuchsia_zircon as zx,
    futures::StreamExt,
    runner::component::ComponentNamespace,
};

/// The name of the collection in which the starnix kernel is instantiated.
pub const RUNNERS_COLLECTION: &str = "runners";

/// Replace the arguments in `program` with `test_arguments`, which were provided to the test
/// framework directly.
pub fn replace_program_args(test_arguments: Vec<String>, program: &mut fdata::Dictionary) {
    update_program_args(test_arguments, program, false);
}

/// Insert `new_args` into the arguments in `program`.
pub fn append_program_args(new_args: Vec<String>, program: &mut fdata::Dictionary) {
    update_program_args(new_args, program, true);
}

/// Instantiates a starnix kernel in the realm of the given namespace.
///
/// # Parameters
///   - `namespace`: The namespace in which to fetch the realm to instantiate the runner in.
///   - `runner_name`: The name of the runner child.
///
/// Returns a proxy to the instantiated runner as well as to the realm in which the runner is
/// instantiated.
pub async fn instantiate_kernel_in_realm(
    namespace: &ComponentNamespace,
    runner_name: &str,
) -> Result<(frunner::ComponentRunnerProxy, fcomponent::RealmProxy), Error> {
    let runner_url = "galaxy#meta/starnix_kernel.cm";

    let realm = get_realm(namespace)?;
    realm
        .create_child(
            &mut fdecl::CollectionRef { name: RUNNERS_COLLECTION.into() },
            fdecl::Child {
                name: Some(runner_name.to_string()),
                url: Some(runner_url.to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                ..fdecl::Child::EMPTY
            },
            fcomponent::CreateChildArgs::EMPTY,
        )
        .await?
        .map_err(|e| anyhow::anyhow!("failed to create runner child: {:?}", e))?;
    let runner_outgoing = open_exposed_directory(&realm, &runner_name, RUNNERS_COLLECTION).await?;
    let starnix_kernel = fclient::connect_to_protocol_at_dir_root::<frunner::ComponentRunnerMarker>(
        &runner_outgoing,
    )?;

    Ok((starnix_kernel, realm))
}

/// Returns numbered handles with their respective stdout and stderr clients.
pub fn create_numbered_handles() -> (Option<Vec<fprocess::HandleInfo>>, zx::Socket, zx::Socket) {
    let (test_stdin, _) = zx::Socket::create(zx::SocketOpts::STREAM).unwrap();
    let (test_stdout, stdout_client) = zx::Socket::create(zx::SocketOpts::STREAM).unwrap();
    let (test_stderr, stderr_client) = zx::Socket::create(zx::SocketOpts::STREAM).unwrap();
    let stdin_handle_info = fprocess::HandleInfo {
        handle: test_stdin.into_handle(),
        id: fruntime::HandleInfo::new(fruntime::HandleType::FileDescriptor, 0).as_raw(),
    };
    let stdout_handle_info = fprocess::HandleInfo {
        handle: test_stdout.into_handle(),
        id: fruntime::HandleInfo::new(fruntime::HandleType::FileDescriptor, 1).as_raw(),
    };
    let stderr_handle_info = fprocess::HandleInfo {
        handle: test_stderr.into_handle(),
        id: fruntime::HandleInfo::new(fruntime::HandleType::FileDescriptor, 2).as_raw(),
    };

    let numbered_handles = Some(vec![stdin_handle_info, stdout_handle_info, stderr_handle_info]);

    (numbered_handles, stdout_client, stderr_client)
}

/// Starts the test component and returns its proxy.
pub fn start_test_component(
    test_url: &str,
    program: Option<fdata::Dictionary>,
    namespace: &ComponentNamespace,
    numbered_handles: Option<Vec<fprocess::HandleInfo>>,
    starnix_kernel: &frunner::ComponentRunnerProxy,
) -> Result<frunner::ComponentControllerProxy, Error> {
    let (component_controller, component_controller_server_end) =
        create_proxy::<frunner::ComponentControllerMarker>()?;
    let ns = Some(ComponentNamespace::try_into(namespace.clone())?);
    let (outgoing_dir, _outgoing_dir) = zx::Channel::create();

    let start_info = frunner::ComponentStartInfo {
        resolved_url: Some(test_url.to_string()),
        program,
        ns,
        outgoing_dir: Some(outgoing_dir.into()),
        runtime_dir: None,
        numbered_handles,
        ..frunner::ComponentStartInfo::EMPTY
    };

    starnix_kernel.start(start_info, component_controller_server_end)?;

    Ok(component_controller)
}

/// Reads the result of the test run from `event_stream`.
///
/// The result is determined by reading the epitaph from the provided `event_stream`.
pub async fn read_result(
    mut event_stream: frunner::ComponentControllerEventStream,
) -> ftest::Result_ {
    let component_epitaph = match event_stream.next().await {
        Some(Err(fidl::Error::ClientChannelClosed { status, .. })) => status,
        result => {
            tracing::error!(
                "Didn't get epitaph from the component controller, instead got: {:?}",
                result
            );
            // Fail the test case here, since the component controller's epitaph couldn't be
            // read.
            zx::Status::INTERNAL
        }
    };

    match component_epitaph {
        zx::Status::OK => {
            ftest::Result_ { status: Some(ftest::Status::Passed), ..ftest::Result_::EMPTY }
        }
        _ => ftest::Result_ { status: Some(ftest::Status::Failed), ..ftest::Result_::EMPTY },
    }
}

/// Replace or append the arguments in `program` with `new_args`.
fn update_program_args(mut new_args: Vec<String>, program: &mut fdata::Dictionary, append: bool) {
    /// The program argument key name.
    const ARGS_KEY: &str = "args";

    if new_args.is_empty() {
        return;
    }

    let mut new_entry = fdata::DictionaryEntry {
        key: ARGS_KEY.to_string(),
        value: Some(Box::new(fdata::DictionaryValue::StrVec(new_args.clone()))),
    };
    if let Some(entries) = &mut program.entries {
        if let Some(index) = entries.iter().position(|entry| entry.key == ARGS_KEY) {
            let entry = entries.remove(index);

            if append {
                if let Some(mut box_value) = entry.value {
                    if let fdata::DictionaryValue::StrVec(ref mut args) = &mut *box_value {
                        args.append(&mut new_args);
                        new_entry.value =
                            Some(Box::new(fdata::DictionaryValue::StrVec(args.to_vec())));
                    };
                }
            }
        }
        entries.push(new_entry);
    } else {
        let entries = vec![new_entry];
        program.entries = Some(entries);
    };
}

fn get_realm(namespace: &ComponentNamespace) -> Result<fcomponent::RealmProxy, Error> {
    namespace
        .items()
        .iter()
        .flat_map(|(s, d)| {
            if s == "/svc" {
                Some(fuchsia_component::client::connect_to_protocol_at_dir_root::<
                    fcomponent::RealmMarker,
                >(&d))
            } else {
                None
            }
        })
        .next()
        .ok_or_else(|| anyhow!("Unable to find /svc"))?
}

async fn open_exposed_directory(
    realm: &fcomponent::RealmProxy,
    child_name: &str,
    collection_name: &str,
) -> Result<fio::DirectoryProxy, Error> {
    let (directory_proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;
    realm
        .open_exposed_dir(
            &mut fdecl::ChildRef {
                name: child_name.into(),
                collection: Some(collection_name.into()),
            },
            server_end,
        )
        .await?
        .map_err(|e| {
            anyhow!(
                "failed to bind to child {} in collection {:?}: {:?}",
                child_name,
                collection_name,
                e
            )
        })?;
    Ok(directory_proxy)
}
