// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::*,
    fidl_fuchsia_examples::EchoMarker,
    fidl_test_example::RealmOptions,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    tracing::info,
};

pub(crate) struct RealmFactory {
    realm_options: Option<RealmOptions>,
}

impl RealmFactory {
    pub fn new() -> Self {
        Self { realm_options: Some(RealmOptions { ..Default::default() }) }
    }
    pub fn set_realm_options(&mut self, options: RealmOptions) -> Result<(), Error> {
        match self.realm_options {
            None => bail!("the realm has already been created"),
            Some(_) => self.realm_options.replace(options),
        };
        Ok(())
    }
    pub async fn create_realm(&mut self) -> Result<RealmInstance, Error> {
        let realm_options = self.realm_options.take().unwrap();
        build_realm(realm_options).await
    }
}

async fn build_realm(options: RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:?}", options);
    let builder = RealmBuilder::new().await?;
    let echo = builder.add_child("echo", "#meta/echo_server.cm", ChildOptions::new()).await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<EchoMarker>())
                .from(&echo)
                .to(Ref::parent()),
        )
        .await?;
    let realm = builder.build().await?;
    Ok(realm)
}
