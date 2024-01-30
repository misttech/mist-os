// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::*;
use fidl::endpoints::create_endpoints;
use fuchsia_component::client::connect_to_protocol;
use realm_proxy_client::{extend_namespace, InstalledNamespace};

pub(crate) async fn create_realm() -> Result<InstalledNamespace, Error> {
    inner_create_realm(fidl_test_sampler::RealmOptions { ..Default::default() }).await
}

pub(crate) async fn create_realm_with_name(
    name: impl Into<String>,
) -> Result<InstalledNamespace, Error> {
    inner_create_realm(fidl_test_sampler::RealmOptions {
        sampler_component_name: Some(name.into()),
        ..Default::default()
    })
    .await
}

async fn inner_create_realm(
    options: fidl_test_sampler::RealmOptions,
) -> Result<InstalledNamespace, Error> {
    let realm_factory = connect_to_protocol::<fidl_test_sampler::RealmFactoryMarker>()?;
    let (dict_client, dict_server) = create_endpoints();
    realm_factory
        .create_realm(options, dict_server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    let ns = extend_namespace(realm_factory, dict_client).await?;
    Ok(ns)
}
