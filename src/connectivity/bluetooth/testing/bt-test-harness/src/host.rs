// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    fidl_fuchsia_bluetooth_host::HostProxy,
    fidl_fuchsia_bluetooth_test::HciEmulatorProxy,
    fuchsia_bluetooth::{
        expectation::{
            asynchronous::{
                expectable, Expectable, ExpectableExt, ExpectableState, ExpectableStateExt,
            },
            Predicate,
        },
        types::{HostInfo, Peer, PeerId},
    },
    futures::{
        future::{self, BoxFuture, Future},
        FutureExt, TryFutureExt,
    },
    hci_emulator_client::Emulator,
    std::{
        collections::HashMap,
        ops::{Deref, DerefMut},
        sync::Arc,
    },
    test_harness::{SharedState, TestHarness},
    tracing::warn,
};

use crate::{
    core_realm::SHARED_STATE_INDEX,
    emulator::{watch_controller_parameters, EmulatorState},
    host_realm::HostRealm,
    timeout_duration,
};

#[derive(Clone, Debug)]
pub struct HostState {
    emulator_state: EmulatorState,

    // Current bt-host component state.
    host_info: HostInfo,

    // All known remote devices, indexed by their identifiers.
    peers: HashMap<PeerId, Peer>,
}

impl HostState {
    pub fn peers(&self) -> &HashMap<PeerId, Peer> {
        &self.peers
    }
    pub fn info(&self) -> &HostInfo {
        &self.host_info
    }
}

impl AsMut<EmulatorState> for HostState {
    fn as_mut(&mut self) -> &mut EmulatorState {
        &mut self.emulator_state
    }
}

impl AsRef<EmulatorState> for HostState {
    fn as_ref(&self) -> &EmulatorState {
        &self.emulator_state
    }
}

/// Auxiliary data for the HostHarness
pub struct Aux {
    pub host: HostProxy,
    pub emulator: HciEmulatorProxy,
}

impl AsRef<HciEmulatorProxy> for Aux {
    fn as_ref(&self) -> &HciEmulatorProxy {
        &self.emulator
    }
}

#[derive(Clone)]
pub struct HostHarness(Expectable<HostState, Aux>);

impl Deref for HostHarness {
    type Target = Expectable<HostState, Aux>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for HostHarness {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl TestHarness for HostHarness {
    type Env = (Emulator, Arc<HostRealm>);
    type Runner = BoxFuture<'static, Result<(), Error>>;

    fn init(
        shared_state: &Arc<SharedState>,
    ) -> BoxFuture<'static, Result<(Self, Self::Env, Self::Runner), Error>> {
        let shared_state = shared_state.clone();
        async move {
            let realm =
                shared_state.get_or_insert_with(SHARED_STATE_INDEX, HostRealm::create).await?;

            let (harness, emulator) = new_host_harness(realm.clone()).await?;

            let watch_info = watch_host_info(harness.clone())
                .map_err(|e| e.context("Error watching host state"))
                .err_into();
            let watch_peers = watch_peers(harness.clone())
                .map_err(|e| e.context("Error watching peers"))
                .err_into();
            let watch_emulator_params = watch_controller_parameters(harness.0.clone())
                .map_err(|e| e.context("Error watching controller parameters"))
                .err_into();

            let run = future::try_join3(watch_info, watch_peers, watch_emulator_params)
                .map_ok(|((), (), ())| ())
                .boxed();
            Ok((harness, (emulator, realm), run))
        }
        .boxed()
    }

    fn terminate(env: Self::Env) -> BoxFuture<'static, Result<(), Error>> {
        // Keep the realm alive until the future resolves.
        async move {
            let (mut emulator, _realm) = env;
            emulator.destroy_and_wait().await
        }
        .boxed()
    }
}

async fn new_host_harness(realm: Arc<HostRealm>) -> Result<(HostHarness, Emulator), Error> {
    // Publishing an HCI device can only occur after HostRealm::create() is called
    let dev_dir = realm.dev().context("failed to open dev directory")?;
    let emulator =
        Emulator::create(dev_dir).await.context("Error creating emulator root device")?;
    let device_path = emulator
        .publish_and_wait_for_device_path(Emulator::default_settings())
        .await
        .context("Error publishing emulator hci device")?;

    let host = HostRealm::create_bt_host_in_collection(&realm, &device_path).await?.into_proxy()?;
    let host_info = host
        .watch_state()
        .await
        .context("Error calling WatchState()")?
        .try_into()
        .context("Invalid HostInfo received")?;

    let peers = HashMap::new();

    let harness = HostHarness(expectable(
        HostState { emulator_state: EmulatorState::default(), host_info, peers },
        Aux { host, emulator: emulator.emulator().clone() },
    ));

    Ok((harness, emulator))
}

async fn watch_peers(harness: HostHarness) -> Result<(), Error> {
    // Clone the proxy so that the aux() lock is not held while waiting.
    let proxy = harness.aux().host.clone();
    loop {
        let (updated, removed) = proxy.watch_peers().await?;
        for peer in updated.into_iter() {
            let peer: Peer = peer.try_into()?;
            let _ = harness.write_state().peers.insert(peer.id.clone(), peer);
            harness.notify_state_changed();
        }
        for id in removed.into_iter() {
            let id = id.into();
            if harness.write_state().peers.remove(&id).is_none() {
                warn!(%id, "HostHarness: Removed id that wasn't present");
            }
        }
    }
}

async fn watch_host_info(harness: HostHarness) -> Result<(), Error> {
    let proxy = harness.aux().host.clone();
    loop {
        let info = proxy.watch_state().await?;
        harness.write_state().host_info = info.try_into()?;
        harness.notify_state_changed();
    }
}

pub mod expectation {
    use super::*;

    /// Returns a Future that resolves when the state of any RemoteDevice matches `target`.
    pub fn peer(
        host: &HostHarness,
        p: Predicate<Peer>,
    ) -> impl Future<Output = Result<HostState, Error>> + '_ {
        host.when_satisfied(
            Predicate::any(p).over_value(
                |host: &HostState| host.peers.values().cloned().collect::<Vec<_>>(),
                ".peers.values()",
            ),
            timeout_duration(),
        )
    }

    /// Returns a Future that resolves when the HostInfo matches `target`.
    pub fn host_state(
        host: &HostHarness,
        p: Predicate<HostInfo>,
    ) -> impl Future<Output = Result<HostState, Error>> + '_ {
        host.when_satisfied(
            p.over(|host: &HostState| &host.host_info, ".host_info"),
            timeout_duration(),
        )
    }

    /// Returns a Future that resolves when a peer matching `id` is not present on the host.
    pub fn no_peer(host: &HostHarness, id: PeerId) -> impl Future<Output = Result<(), Error>> + '_ {
        host.when_satisfied(
            Predicate::all(Predicate::not_equal(id)).over_value(
                |host: &HostState| host.peers.keys().cloned().collect::<Vec<_>>(),
                ".peers.keys()",
            ),
            timeout_duration(),
        )
        .map_ok(|_| ())
    }
}
