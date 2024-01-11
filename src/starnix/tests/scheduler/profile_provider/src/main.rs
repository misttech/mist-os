// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_component::BinderMarker;
use fidl_fuchsia_scheduler::{
    ProfileProviderMarker, ProfileProviderRequest, ProfileProviderRequestStream,
    ProfileProviderSetProfileByRoleResponder,
};
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmBuilderParams, Ref, Route,
};
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::{
    channel::mpsc::{UnboundedReceiver, UnboundedSender},
    StreamExt,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use tracing::info;

#[fuchsia::main]
async fn main() {
    info!("reading package profile config");
    let profiles_config = std::fs::read_to_string("/pkg/config/profiles/starnix.profiles").unwrap();
    let profiles_config: ProfilesConfig = serde_json5::from_str(&profiles_config).unwrap();
    let (fake_provider, mut requests) = FakeProfileProvider::new(profiles_config);

    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new().from_relative_url("#meta/test_realm.cm"),
    )
    .await
    .unwrap();

    let profile_provider_ref = builder
        .add_local_child(
            "fake_profile_provider",
            move |handles| Box::pin(fake_provider.clone().serve(handles)),
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

    info!("building realm and starting eager container");
    let realm = builder.build().await.unwrap();

    // initial kernel thread probes for profile provider (zircon 16)
    requests.with_next(|_, role| assert_eq!(role, "fuchsia.starnix.fair.16")).await;

    // init process' initial thread gets default (nice 0, zircon 16) when it starts
    requests.with_next(|_, role| assert_eq!(role, "fuchsia.starnix.fair.16")).await;

    info!("kernel and container init have requested thread roles, manually starting puppet");
    let _binder = realm
        .root
        .connect_to_named_protocol_at_exposed_dir::<BinderMarker>("PuppetBinder")
        .unwrap();

    // * puppet main process
    //   * thread one gets default (nice 0, zircon 16) when it starts
    //     * this thread requests an update (nice 10, zircon 11)
    //   * thread two in this process starts, inherits (nice 10, zircon 11)
    //     * this thread requests an update (nice 12, zircon 10)
    let puppet_thread_one_koid = requests
        .with_next(|koid, role| {
            assert_eq!(role, "fuchsia.starnix.fair.16");
            koid
        })
        .await;
    requests
        .with_next(|koid, role| {
            assert_eq!(koid, puppet_thread_one_koid);
            assert_eq!(role, "fuchsia.starnix.fair.11");
        })
        .await;
    let puppet_thread_two_koid = requests
        .with_next(|koid, role| {
            assert_ne!(koid, puppet_thread_one_koid, "request must come from a different thread");
            assert_eq!(role, "fuchsia.starnix.fair.11");
            koid
        })
        .await;
    requests
        .with_next(|koid, role| {
            assert_eq!(koid, puppet_thread_two_koid);
            assert_eq!(role, "fuchsia.starnix.fair.10");
        })
        .await;

    // * puppet child process
    //   * initial thread inherits main process' value (nice 10, zircon 11)
    //     * this thread requests an update (nice 14, zircon 9)
    //   * second thread inherit's main thread's value (nice 14, zircon 9)
    //     * this thread requests an update (nice 16, zircon 8)
    let puppet_child_thread_one_koid = requests
        .with_next(|koid, role| {
            assert_eq!(role, "fuchsia.starnix.fair.11");
            koid
        })
        .await;
    requests
        .with_next(|koid, role| {
            assert_eq!(koid, puppet_child_thread_one_koid);
            assert_eq!(role, "fuchsia.starnix.fair.9");
        })
        .await;

    let puppet_child_thread_two_koid = requests
        .with_next(|koid, role| {
            assert_eq!(role, "fuchsia.starnix.fair.9");
            koid
        })
        .await;
    requests
        .with_next(|koid, role| {
            assert_eq!(koid, puppet_child_thread_two_koid);
            assert_eq!(role, "fuchsia.starnix.fair.8");
        })
        .await;

    realm.destroy().await.unwrap();
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

#[derive(Clone)]
struct FakeProfileProvider {
    config: ProfilesConfig,
    sender: UnboundedSender<SetProfileByRole>,
}

impl FakeProfileProvider {
    fn new(config: ProfilesConfig) -> (Self, FakeProfileRequests) {
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        (Self { config, sender }, FakeProfileRequests { receiver })
    }

    async fn serve(self, handles: LocalComponentHandles) -> Result<(), anyhow::Error> {
        let mut fs = fuchsia_component::server::ServiceFs::new();
        fs.dir("svc").add_fidl_service(|client: ProfileProviderRequestStream| client);
        fs.serve_connection(handles.outgoing_dir).unwrap();

        while let Some(mut client) = fs.next().await {
            while let Some(request) = client.next().await {
                match request.unwrap() {
                    ProfileProviderRequest::SetProfileByRole { handle, role, responder } => {
                        assert!(
                            self.config.profiles.contains_key(&role),
                            "requested={role} allowed={:#?}",
                            self.config.profiles,
                        );

                        self.sender
                            .unbounded_send(SetProfileByRole { handle, role, responder })
                            .unwrap();
                    }
                    other => {
                        panic!("unexpected ProfileProvider request from starnix kernel {other:?}")
                    }
                }
            }
        }

        Ok(())
    }
}

struct FakeProfileRequests {
    receiver: UnboundedReceiver<SetProfileByRole>,
}

impl FakeProfileRequests {
    async fn with_next<R>(&mut self, op: impl FnOnce(zx::Koid, &str) -> R) -> R {
        let next = self.receiver.next().await.unwrap();
        let ret = op(next.handle.get_koid().unwrap(), &next.role);
        next.responder.send(zx::sys::ZX_OK).unwrap();
        ret
    }
}

#[derive(Debug)]
struct SetProfileByRole {
    handle: zx::Handle,
    role: String,
    responder: ProfileProviderSetProfileByRoleResponder,
}
