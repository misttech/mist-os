// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::*;
use fidl::endpoints::create_endpoints;
use fuchsia_component::client::connect_to_protocol;
use realm_proxy::client::RealmProxyClient;

pub(crate) async fn create_realm() -> Result<RealmProxyClient, Error> {
    create_realm_(fidl_test_sampler::RealmOptions { ..Default::default() }).await
}

pub(crate) async fn create_realm_with_name(
    name: impl Into<String>,
) -> Result<RealmProxyClient, Error> {
    create_realm_(fidl_test_sampler::RealmOptions {
        realm_name: Some(name.into()),
        ..Default::default()
    })
    .await
}

async fn create_realm_(
    options: fidl_test_sampler::RealmOptions,
) -> Result<RealmProxyClient, Error> {
    let realm_factory = connect_to_protocol::<fidl_test_sampler::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    realm_factory.set_realm_options(options).await?.map_err(realm_proxy::Error::OperationError)?;
    realm_factory.create_realm(server).await?.map_err(realm_proxy::Error::OperationError)?;
    Ok(RealmProxyClient::from(client))
}
