// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl_fuchsia_bluetooth_sys::BootstrapProxy;
use fuchsia_bluetooth::expectation::asynchronous::{expectable, Expectable};
use futures::future::{self, BoxFuture, FutureExt};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use test_harness::{SharedState, TestHarness, SHARED_STATE_TEST_COMPONENT_INDEX};

use crate::core_realm::{CoreRealm, SHARED_STATE_INDEX};
use crate::host_watcher::ActivatedFakeHost;

#[derive(Clone)]
pub struct BootstrapHarness(Expectable<(), BootstrapProxy>);

impl Deref for BootstrapHarness {
    type Target = Expectable<(), BootstrapProxy>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for BootstrapHarness {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl TestHarness for BootstrapHarness {
    type Env = (ActivatedFakeHost, Arc<CoreRealm>);
    type Runner = future::Pending<Result<(), Error>>;

    fn init(
        shared_state: &Arc<SharedState>,
    ) -> BoxFuture<'static, Result<(Self, Self::Env, Self::Runner), Error>> {
        let shared_state = shared_state.clone();
        async move {
            let test_component: Arc<String> = shared_state
                .get(SHARED_STATE_TEST_COMPONENT_INDEX)
                .expect("SharedState must have TEST-COMPONENT")?;
            let inserter = move || CoreRealm::create(test_component.to_string());
            let realm = shared_state.get_or_insert_with(SHARED_STATE_INDEX, inserter).await?;
            let fake_host = ActivatedFakeHost::new(realm.clone()).await?;
            let proxy = realm
                .instance()
                .connect_to_protocol_at_exposed_dir()
                .context("Failed to connect to bootstrap service")?;
            Ok((
                BootstrapHarness(expectable(Default::default(), proxy)),
                (fake_host, realm),
                future::pending(),
            ))
        }
        .boxed()
    }

    fn terminate((emulator, realm): Self::Env) -> BoxFuture<'static, Result<(), Error>> {
        // The realm must be kept alive in order for emulator.release() to work properly.
        async move {
            let _realm = realm;
            emulator.release().await
        }
        .boxed()
    }
}
