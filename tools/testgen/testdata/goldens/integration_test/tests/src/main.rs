// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fuchsia_component::client::connect_to_protocol;
use fidl::endpoints::create_endpoints;
use fidl_test_examplecomponent as ftest;
use realm_proxy::client::RealmProxyClient;
use tracing::info;

async fn create_realm(options: ftest::RealmOptions) -> Result<RealmProxyClient> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();

    realm_factory.set_realm_options(options).await?.map_err(realm_proxy::Error::OperationError)?;
    realm_factory.create_realm(server).await?.map_err(realm_proxy::Error::OperationError)?;

    Ok(RealmProxyClient::from(client))
}

#[fuchsia::test]
async fn test_example() -> Result<()> {
    let realm_options = ftest::RealmOptions{ ..Default::default() };
    let _realm = create_realm(realm_options).await?;

    info!("connected to the test realm!");

    Ok(())
}
