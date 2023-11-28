// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_scheduler::{
    ProfileProviderMarker, ProfileProviderRequest, ProfileProviderRequestStream,
};
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmBuilderParams, Ref, Route,
};
use fuchsia_fs::OpenFlags;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::StreamExt;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tracing::info;

#[fuchsia::main]
async fn main() {
    info!("reading package profile config");
    let profiles_config = std::fs::read_to_string("/pkg/config/profiles/starnix.profiles").unwrap();
    let profiles_config: ProfilesConfig = serde_json5::from_str(&profiles_config).unwrap();

    let fence_path_in_container = "/tmp/puppet.done";

    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new().from_relative_url("#meta/test_realm.cm"),
    )
    .await
    .unwrap();

    let roles_requested = Arc::new(Mutex::new(Vec::<(String, zx::Koid)>::new()));

    let roles_recorder = roles_requested.clone();
    let profile_provider_ref = builder
        .add_local_child(
            "fake_profile_provider",
            move |handles: LocalComponentHandles| {
                Box::pin(fake_profile_provider(
                    profiles_config.clone(),
                    roles_recorder.clone(),
                    handles,
                ))
            },
            ChildOptions::new(),
        )
        .await
        .unwrap();

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(ProfileProviderMarker::PROTOCOL_NAME))
                .from(&profile_provider_ref)
                .to(Ref::child("kernel")),
        )
        .await
        .unwrap();

    info!("building realm and starting eager puppet");
    let realm = builder.build().await.unwrap();

    info!("opening test realm's exposed directory");
    let container_root = fuchsia_fs::directory::open_directory(
        realm.root.get_exposed_dir(),
        "fs_root",
        fuchsia_fs::OpenFlags::RIGHT_READABLE | fuchsia_fs::OpenFlags::DIRECTORY,
    )
    .await
    .unwrap();

    info!("waiting for puppet to write to done file");
    'wait_for_puppet_done: loop {
        match fuchsia_fs::directory::open_file(
            &container_root,
            &fence_path_in_container,
            OpenFlags::RIGHT_READABLE,
        )
        .await
        {
            Ok(f) => {
                if fuchsia_fs::file::read_to_string(&f).await.unwrap() == "done!" {
                    break 'wait_for_puppet_done;
                }
            }
            Err(fuchsia_fs::node::OpenError::OpenError(zx::Status::NOT_FOUND)) => {
                fuchsia_async::Timer::new(std::time::Duration::from_millis(250)).await;
                continue 'wait_for_puppet_done;
            }
            Err(e) => panic!("unexpected error waiting for puppet to mark itself done: {e:#?}"),
        }
    }

    realm.destroy().await.unwrap();

    let roles_requested = &*roles_requested.lock().unwrap();

    assert_eq!(
        roles_requested.len(),
        10,
        "should have had 10 role requests, got {roles_requested:?}"
    );

    // initial kernel thread probes for profile provider (zircon 16)
    let (kernel_probe_role, _) = &roles_requested[0];
    assert_eq!(kernel_probe_role, "fuchsia.starnix.fair.16");

    // init process' initial thread gets default (nice 0, zircon 16) when it starts
    let (init_process_role, _) = &roles_requested[1];
    assert_eq!(init_process_role, "fuchsia.starnix.fair.16");

    // * puppet main process
    //   * thread one gets default (nice 0, zircon 16) when it starts
    //     * this thread requests an update (nice 10, zircon 11)
    //   * thread two in this process starts, inherits (nice 10, zircon 11)
    //     * this thread requests an update (nice 12, zircon 10)
    let (puppet_main_thread_one_initial_role, puppet_main_thread_one_koid) = &roles_requested[2];
    assert_eq!(puppet_main_thread_one_initial_role, "fuchsia.starnix.fair.16");
    let (puppet_main_thread_one_second_role, puppet_main_thread_one_koid2) = &roles_requested[3];
    assert_eq!(puppet_main_thread_one_koid, puppet_main_thread_one_koid2);
    assert_eq!(puppet_main_thread_one_second_role, "fuchsia.starnix.fair.11");

    let (puppet_main_thread_two_initial_role, puppet_main_thread_two_koid) = &roles_requested[4];
    assert_eq!(puppet_main_thread_two_initial_role, "fuchsia.starnix.fair.11");
    let (puppet_main_thread_two_second_role, puppet_main_thread_two_koid2) = &roles_requested[5];
    assert_eq!(puppet_main_thread_two_koid, puppet_main_thread_two_koid2);
    assert_eq!(puppet_main_thread_two_second_role, "fuchsia.starnix.fair.10");

    // * puppet child process
    //   * initial thread inherits main process' value (nice 10, zircon 11)
    //     * this thread requests an update (nice 14, zircon 9)
    //   * second thread inherit's main thread's value (nice 14, zircon 9)
    //     * this thread requests an update (nice 16, zircon 8)
    let (puppet_child_thread_one_initial_role, puppet_child_thread_one_koid) = &roles_requested[6];
    assert_eq!(puppet_child_thread_one_initial_role, "fuchsia.starnix.fair.11");
    let (puppet_child_thread_one_second_role, puppet_child_thread_one_koid2) = &roles_requested[7];
    assert_eq!(puppet_child_thread_one_koid, puppet_child_thread_one_koid2);
    assert_eq!(puppet_child_thread_one_second_role, "fuchsia.starnix.fair.9");

    let (puppet_child_thread_two_initial_role, puppet_child_thread_two_koid) = &roles_requested[8];
    assert_eq!(puppet_child_thread_two_initial_role, "fuchsia.starnix.fair.9");
    let (puppet_child_thread_two_second_role, puppet_child_thread_two_koid2) = &roles_requested[9];
    assert_eq!(puppet_child_thread_two_koid, puppet_child_thread_two_koid2);
    assert_eq!(puppet_child_thread_two_second_role, "fuchsia.starnix.fair.8");
}

#[derive(Clone, Debug, Deserialize)]
struct ProfilesConfig {
    profiles: BTreeMap<String, ProfileConfig>,
}

#[derive(Clone, Debug, Deserialize)]
struct ProfileConfig {
    #[allow(unused)]
    priority: u8,
}

async fn fake_profile_provider(
    known_profiles: ProfilesConfig,
    roles_requested: Arc<Mutex<Vec<(String, zx::Koid)>>>,
    handles: LocalComponentHandles,
) -> Result<(), anyhow::Error> {
    let mut fs = fuchsia_component::server::ServiceFs::new();
    fs.dir("svc").add_fidl_service(|client: ProfileProviderRequestStream| client);
    fs.serve_connection(handles.outgoing_dir).unwrap();

    while let Some(mut client) = fs.next().await {
        while let Some(request) = client.next().await {
            match request.unwrap() {
                ProfileProviderRequest::SetProfileByRole { handle, role, responder } => {
                    assert!(
                        known_profiles.profiles.contains_key(&role),
                        "requested={role} allowed={known_profiles:#?}"
                    );
                    roles_requested.lock().unwrap().push((role, handle.get_koid().unwrap()));
                    responder.send(zx::Status::OK.into_raw()).unwrap();
                }
                other => panic!("unexpected ProfileProvider request from starnix kernel {other:?}"),
            }
        }
    }

    Ok(())
}
