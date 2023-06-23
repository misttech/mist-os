// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::*,
    fidl_test_mock as ftest,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    tracing::info,
};

pub(crate) struct RealmFactoryImpl {
    realm_options: Option<ftest::RealmOptions>,
}

impl RealmFactoryImpl {
    pub fn new() -> Self {
        Self { realm_options: Some(ftest::RealmOptions { ..Default::default() }) }
    }

    pub fn set_realm_options(&mut self, options: ftest::RealmOptions) -> Result<(), Error> {
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

async fn build_realm(options: ftest::RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:?}", options);

    let builder = RealmBuilder::new().await?;

    let mock = builder.add_child("mock", "hwinfo-mock#meta/mock.cm", ChildOptions::new()).await?;
    for protocol in vec![
        "fuchsia.hwinfo.Board",
        "fuchsia.hwinfo.Product",
        "fuchsia.hwinfo.Device",
        "fuchsia.hwinfo.mock.Setter",
    ] {
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name(protocol))
                    .from(&mock)
                    .to(Ref::parent()),
            )
            .await?;
    }

    let realm = builder.build().await?;
    Ok(realm)
}
