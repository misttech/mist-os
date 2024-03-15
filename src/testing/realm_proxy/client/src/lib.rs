// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{bail, format_err, Result},
    fdio::Namespace,
    fidl::endpoints::{
        create_endpoints, ClientEnd, DiscoverableProtocolMarker, Proxy, ServiceMarker, ServiceProxy,
    },
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_testing_harness::{RealmProxy_Marker, RealmProxy_Proxy},
    fuchsia_component::client::connect_to_protocol,
    std::fmt::Debug,
    uuid::Uuid,
};

pub mod error;
pub use error::Error;

/// A thin wrapper that represents the namespace created by [extend_namespace].
///
/// Users can obtain the path to the namespace from [InstalledNamespace::prefix] and pass that
/// to capability connection APIs such as [fuchsia-component]'s [client::connect_to_protocol_at]
/// to access capabilities in the namespace.
///
/// Furthermore, the [InstalledNamespace] acts as an RAII container for the capabilities. When
/// the [InstalledNamespace] is dropped, the test realm factory server may free state associated
/// with serving those capabilities. Therefore, the test should only drop this once it no longer
/// needs to connect to the capabilities or needs activity performed on their behalf.
pub struct InstalledNamespace {
    prefix: String,

    /// This is not used, but it keeps the RealmFactory connection alive.
    ///
    /// The RealmFactory server may use this connection to pin the lifetime of the realm created
    /// for the test.
    _realm_factory: fidl::AsyncChannel,
}

impl InstalledNamespace {
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

impl Drop for InstalledNamespace {
    fn drop(&mut self) {
        let Ok(namespace) = Namespace::installed() else {
            return;
        };
        let _ = namespace.unbind(&self.prefix);
    }
}

/// Converts the given dictionary to a namespace and adds it this component's namespace,
/// thinly wrapped by the returned [InstalledNamespace].
///
/// Users can obtain the path to the namespace from [InstalledNamespace::prefix] and pass that
/// to capability connection APIs such as [fuchsia-component]'s [client::connect_to_protocol_at]
/// to access capabilities in the namespace.
///
/// Furthermore, the [InstalledNamespace] acts as an RAII container for the capabilities. When
/// the [InstalledNamespace] is dropped, the test realm factory server may free state associated
/// with serving those capabilities. Therefore, the test should only drop this once it no longer
/// needs to connect to the capabilities or needs activity performed on their behalf.
pub async fn extend_namespace<T>(
    realm_factory: T,
    dictionary: ClientEnd<fsandbox::DictionaryMarker>,
) -> Result<InstalledNamespace>
where
    T: Proxy + Debug,
{
    let namespace_proxy = connect_to_protocol::<fcomponent::NamespaceMarker>()?;
    // TODO: What should we use for the namespace's unique id? Could also
    // consider an atomic counter, or the name of the test
    let prefix = format!("/dict-{}", Uuid::new_v4());
    let dicts = vec![fcomponent::NamespaceInputEntry { path: prefix.clone().into(), dictionary }];
    let mut namespace_entries =
        namespace_proxy.create(dicts).await?.map_err(|e| format_err!("{:?}", e))?;
    let namespace = Namespace::installed()?;
    let count = namespace_entries.len();
    if count != 1 {
        bail!(
            "namespace {prefix} should have exactly one entry but it has {count}. This suggests a \
            bug in the namespace protocol. {namespace_entries:?}"
        );
    }
    let entry = namespace_entries.remove(0);
    if entry.path.is_none() || entry.directory.is_none() {
        bail!(
            "namespace {prefix} contains incomplete entry. This suggests a bug in the namespace \
            protocol {entry:?}"
        );
    }
    if entry.path.as_ref().unwrap() != &prefix {
        bail!(
            "namespace {prefix} does not match path. This suggests a bug in the namespace protocol. \
            {entry:?}"
        );
    }
    namespace.bind(&prefix, entry.directory.unwrap())?;
    Ok(InstalledNamespace { prefix, _realm_factory: realm_factory.into_channel().unwrap() })
}

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
    pub fn connect() -> Result<Self, anyhow::Error> {
        let inner = connect_to_protocol::<RealmProxy_Marker>()?;
        Ok(Self { inner })
    }

    // Connects to the protocol marked by [T] via the proxy.
    //
    // Returns an error if the connection fails.
    pub async fn connect_to_protocol<T: DiscoverableProtocolMarker>(
        &self,
    ) -> Result<T::Proxy, anyhow::Error> {
        self.connect_to_named_protocol::<T>(T::PROTOCOL_NAME).await
    }

    // Connects to the protocol with the given name, via the proxy.
    //
    // Returns an error if the connection fails.
    pub async fn connect_to_named_protocol<T: DiscoverableProtocolMarker>(
        &self,
        protocol_name: &str,
    ) -> Result<T::Proxy, anyhow::Error> {
        let (client, server) = create_endpoints::<T>();
        let res =
            self.inner.connect_to_named_protocol(protocol_name, server.into_channel()).await?;

        if let Some(op_err) = res.err() {
            bail!("{:?}", op_err);
        }

        Ok(client.into_proxy()?)
    }

    // Opens the given service capability, via the proxy.
    //
    // See https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities/service
    // for more information about service capabilities.
    //
    // Returns an error if the connection fails.
    pub async fn open_service<T: ServiceMarker>(
        &self,
    ) -> Result<fio::DirectoryProxy, anyhow::Error> {
        let (client, server) = create_endpoints::<fio::DirectoryMarker>();
        let res = self.inner.open_service(T::SERVICE_NAME, server.into_channel()).await?;
        if let Some(op_err) = res.err() {
            bail!("{:?}", op_err);
        }

        Ok(client.into_proxy()?)
    }

    // Connects to the given service instance, via the proxy.
    //
    // See https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities/service
    // for more information about service capabilities.
    //
    // Returns an error if the connection fails.
    pub async fn connect_to_service_instance<T: ServiceMarker>(
        &self,
        instance: &str,
    ) -> Result<T::Proxy, anyhow::Error> {
        let (client, server) = create_endpoints::<fio::DirectoryMarker>();
        let res = self
            .inner
            .connect_to_service_instance(T::SERVICE_NAME, instance, server.into_channel())
            .await?;
        if let Some(op_err) = res.err() {
            bail!("{:?}", op_err);
        }

        Ok(T::Proxy::from_member_opener(Box::new(
            fuchsia_component::client::ServiceInstanceDirectory(client.into_proxy()?),
        )))
    }
}
