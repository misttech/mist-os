// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl::endpoints::create_endpoints;
use fidl_fidl_examples_routing_echo as fecho;
use fidl_test_echoserver as ftest;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at};
use realm_proxy::client::{extend_namespace, InstalledNamespace};
use tracing::info;

async fn create_realm(options: ftest::RealmOptions) -> Result<InstalledNamespace> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (dict_client, dict_server) = create_endpoints();

    realm_factory
        .create_realm(options, dict_server)
        .await?
        .map_err(realm_proxy::Error::OperationError)?;
    let ns = extend_namespace(dict_client).await?;

    Ok(ns)
}

#[fuchsia::test]
async fn test_example() -> Result<()> {
    let realm_options = ftest::RealmOptions { ..Default::default() };
    let test_ns = create_realm(realm_options).await?;

    info!("connected to the test realm!");

    let echo = connect_to_protocol_at::<fecho::EchoMarker>(test_ns.prefix())?;
    let response = echo.echo_string(Some("hello")).await.unwrap().unwrap();
    assert_eq!(response, "hello");
    Ok(())
}
