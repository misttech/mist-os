// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{bail, Error, Result},
    fidl::endpoints::{create_endpoints, ClientEnd, DiscoverableProtocolMarker},
    fidl_fuchsia_testing_harness::{RealmProxy_Marker, RealmProxy_Proxy},
    fuchsia_component::client::connect_to_protocol,
};

// RealmProxyClient is a client for fuchsia.testing.harness.RealmProxy.
//
// The calling component must have a handle to the RealmProxy protocol in
// order to use this struct. Once the caller has connected to the RealmProxy
// service, they can access the other services in the proxied test realm by
// calling [connect_to_protocol].
//
// # Example Usage
//
// ```
// let realm_proxy = RealmProxyClient::connect()?;
// let echo = realm_proxy.connect_to_protocol::<EchoMarker>().await?;
// ```
pub struct RealmProxyClient {
    inner: RealmProxy_Proxy,
}

impl From<RealmProxy_Proxy> for RealmProxyClient {
    fn from(value: RealmProxy_Proxy) -> Self {
        Self { inner: value }
    }
}

impl From<ClientEnd<RealmProxy_Marker>> for RealmProxyClient {
    fn from(value: ClientEnd<RealmProxy_Marker>) -> Self {
        let inner = value.into_proxy().expect("ClientEnd::into_proxy");
        Self { inner }
    }
}

impl RealmProxyClient {
    // Connects to the RealmProxy service.
    pub fn connect() -> Result<Self, Error> {
        let inner = connect_to_protocol::<RealmProxy_Marker>()?;
        Ok(Self { inner })
    }

    // Connects to the protocol marked by [T] via the proxy.
    //
    // Returns an error if the connection fails.
    pub async fn connect_to_protocol<T: DiscoverableProtocolMarker>(
        &self,
    ) -> Result<T::Proxy, Error> {
        self.connect_to_named_protocol::<T>(T::PROTOCOL_NAME).await
    }

    // Connects to the protocol with the given name, via the proxy.
    //
    // Returns an error if the connection fails.
    pub async fn connect_to_named_protocol<T: DiscoverableProtocolMarker>(
        &self,
        protocol_name: &str,
    ) -> Result<T::Proxy, Error> {
        let (client, server) = create_endpoints::<T>();
        let res =
            self.inner.connect_to_named_protocol(protocol_name, server.into_channel()).await?;

        if let Some(op_err) = res.err() {
            bail!("{:?}", op_err);
        }

        Ok(client.into_proxy()?)
    }
}
