// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides a transparent netstack proxy.
//!
//! The netstack proxy reads the network stack version it wants to use from
//! fuchsia.net.stackmigrationdeprecated.Control and spawns the appropriate
//! netstack binary from its own package.
//!
//! The directory request handle is passed directly to the spawned netstack.
//!
//! The incoming namespace for the spawned netstack is carefully constructed to
//! extract out the capabilities that are routed to netstack-proxy that are not
//! used by netstack itself.

use cstr::cstr;
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_net_stackmigrationdeprecated as fnet_migration;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx};
use futures::FutureExt as _;
use vfs::directory::{entry::DirectoryEntry, helper::DirectlyMutable};

#[fasync::run_singlethreaded]
pub async fn main() {
    // Start by getting the Netstack version we should use.
    let current_boot_version = {
        let migration =
            fuchsia_component::client::connect_to_protocol::<fnet_migration::StateMarker>()
                .expect("connect to protocol");
        let fnet_migration::InEffectVersion { current_boot, .. } =
            migration.get_netstack_version().await.expect("failed to read netstack version");
        current_boot
    };

    println!("netstack migration proxy using version {current_boot_version:?}");
    let bin_path = match current_boot_version {
        fnet_migration::NetstackVersion::Netstack2 => {
            cstr!("/pkg/bin/netstack")
        }
        fnet_migration::NetstackVersion::Netstack3 => {
            cstr!("/pkg/bin/netstack3")
        }
    };

    let ns = fdio::Namespace::installed().expect("failed to get namespace");
    let mut entries = ns
        .export()
        .expect("failed to export namespace entries")
        .into_iter()
        .filter_map(|fdio::NamespaceEntry { handle, path }| match path.as_str() {
            "/" => {
                panic!("unexpected non flat namespace, bad capabilities will bleed into netstack")
            }
            "/svc" => None,
            x => {
                Some((Some(handle), std::ffi::CString::new(x).expect("failed to create C string")))
            }
        })
        .collect::<Vec<_>>();

    let handle =
        fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleType::DirectoryRequest.into())
            .expect("missing startup handle");

    let mut actions = vec![fdio::SpawnAction::add_handle(
        fuchsia_runtime::HandleInfo::new(fuchsia_runtime::HandleType::DirectoryRequest, 0),
        handle,
    )];

    actions.extend(entries.iter_mut().map(|(handle, path)| {
        // Handle is always Some here, we use an option so we can take it from
        // entries while entries keeps the CString backing.
        let handle = handle.take().unwrap();
        fdio::SpawnAction::add_namespace_entry(path.as_c_str(), handle)
    }));

    let svc = vfs::directory::immutable::simple::simple();
    for s in std::fs::read_dir("/svc").expect("failed to get /svc entries") {
        let entry = s.expect("failed to get directory entry");
        let name = entry.file_name();
        let name = name.to_str().expect("failed to get file name");

        // Don't allow Netstack to see the services that we use exclusively to
        // enable proxying.
        let block_services = [
            fidl_fuchsia_process::LauncherMarker::PROTOCOL_NAME,
            fnet_migration::StateMarker::PROTOCOL_NAME,
        ];
        if block_services.into_iter().any(|s| s == name) {
            continue;
        }
        svc.add_entry(
            name,
            vfs::service::endpoint(move |_, channel| {
                fuchsia_component::client::connect_channel_to_protocol_at_path(
                    channel.into(),
                    entry.path().to_str().expect("failed to get entry path"),
                )
                .unwrap_or_else(|e| eprintln!("error connecting to protocol {:?}", e));
            }),
        )
        .unwrap_or_else(|e| panic!("failed to add entry {name}: {e:?}"));
    }

    let scope = vfs::execution_scope::ExecutionScope::new();

    let (svc_dir, server_end) = fidl::endpoints::create_endpoints::<fidl_fuchsia_io::NodeMarker>();

    svc.open(
        scope.clone(),
        fidl_fuchsia_io::OpenFlags::RIGHT_READABLE
            | fidl_fuchsia_io::OpenFlags::RIGHT_WRITABLE
            | fidl_fuchsia_io::OpenFlags::RIGHT_EXECUTABLE,
        vfs::path::Path::dot(),
        server_end,
    );

    actions
        .push(fdio::SpawnAction::add_namespace_entry(cstr!("/svc"), svc_dir.into_channel().into()));

    let proc = fdio::spawn_etc(
        &fuchsia_runtime::job_default(),
        fdio::SpawnOptions::CLONE_ALL - fdio::SpawnOptions::CLONE_NAMESPACE,
        bin_path,
        &[bin_path],
        None,
        &mut actions[..],
    )
    .expect("failed to spawn netstack");

    let process_terminated = fasync::OnSignals::new(&proc, zx::Signals::PROCESS_TERMINATED).then(
        |r| -> futures::future::Ready<()> {
            let _: zx::Signals = r.expect("failed to observe process termination signals");
            panic!("netstack exited unexpectedly");
        },
    );
    let scope_finished = scope.wait().then(|()| -> futures::future::Ready<()> {
        panic!("vfs scope exited unexpectedly");
    });

    futures::future::join(process_terminated, scope_finished).await;
}
