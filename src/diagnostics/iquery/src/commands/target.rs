// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::commands::types::DiagnosticsProvider;
use crate::commands::utils::*;
use crate::types::Error;
use anyhow::anyhow;
use component_debug::dirs::*;
use diagnostics_data::{Data, DiagnosticsData};
use diagnostics_reader::{ArchiveReader, RetryConfig};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_diagnostics::{ArchiveAccessorMarker, ArchiveAccessorProxy, Selector};
use fidl_fuchsia_io::DirectoryProxy;
use fidl_fuchsia_sys2 as fsys2;
use fuchsia_component::client;
use moniker::Moniker;

static ROOT_ARCHIVIST: &str = "bootstrap/archivist";

pub struct ArchiveAccessorProvider {
    query_proxy: fsys2::RealmQueryProxy,
}

impl ArchiveAccessorProvider {
    pub fn new(proxy: fsys2::RealmQueryProxy) -> Self {
        Self { query_proxy: proxy }
    }
}

impl DiagnosticsProvider for ArchiveAccessorProvider {
    async fn snapshot<D>(
        &self,
        accessor: Option<&str>,
        selectors: impl IntoIterator<Item = Selector>,
    ) -> Result<Vec<Data<D>>, Error>
    where
        D: DiagnosticsData,
    {
        let archive = connect_to_accessor_selector(accessor, &self.query_proxy).await?;
        ArchiveReader::new()
            .with_archive(archive)
            .retry(RetryConfig::never())
            .add_selectors(selectors.into_iter())
            .snapshot::<D>()
            .await
            .map_err(Error::Fetch)
    }

    async fn get_accessor_paths(&self) -> Result<Vec<String>, Error> {
        get_accessor_selectors(&self.query_proxy).await
    }

    fn realm_query(&self) -> &fsys2::RealmQueryProxy {
        &self.query_proxy
    }
}

/// Connect to `fuchsia.diagnostics.*ArchivistAccessor` with the provided selector string.
/// The selector string should be in the form of "<moniker>:<service_name>".
/// If no selector string is provided, it will try to connect to
/// `bootstrap/archivist:fuchsia.diagnostics.ArchiveAccessor`.
pub async fn connect_to_accessor_selector(
    selector: Option<&str>,
    query_proxy: &fsys2::RealmQueryProxy,
) -> Result<ArchiveAccessorProxy, Error> {
    match selector {
        Some(s) => {
            let Some((component, accessor_name)) = s.rsplit_once(":") else {
                return Err(Error::invalid_accessor(s));
            };
            let Ok(moniker) = Moniker::try_from(component) else {
                return Err(Error::invalid_accessor(s));
            };
            connect_accessor(&moniker, accessor_name, query_proxy).await
        }
        None => {
            let moniker = Moniker::try_from(ROOT_ARCHIVIST).unwrap();
            connect_accessor(&moniker, ArchiveAccessorMarker::PROTOCOL_NAME, query_proxy).await
        }
    }
}

// Use the provided `Selector` and depending on the selector,
// opens the `expose` directory and return the proxy to it.
async fn get_dir_proxy(
    moniker: &Moniker,
    proxy: &fsys2::RealmQueryProxy,
) -> Result<DirectoryProxy, Error> {
    let directory_proxy = open_instance_dir_root_readable(moniker, OpenDirType::Exposed, proxy)
        .await
        .map_err(|e| Error::CommunicatingWith("RealmQuery".to_owned(), anyhow!("{:?}", e)))?;
    Ok(directory_proxy)
}

/// Attempt to connect to the `fuchsia.diagnostics.*ArchiveAccessor` with the selector
/// specified.
pub async fn connect_accessor(
    moniker: &Moniker,
    accessor_name: &str,
    proxy: &fsys2::RealmQueryProxy,
) -> Result<ArchiveAccessorProxy, Error> {
    let directory_proxy = get_dir_proxy(moniker, proxy).await?;
    let proxy = client::connect_to_named_protocol_at_dir_root::<ArchiveAccessorMarker>(
        &directory_proxy,
        &accessor_name,
    )
    .map_err(|e| Error::ConnectToProtocol(accessor_name.to_string(), anyhow!("{:?}", e)))?;
    Ok(proxy)
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use iquery_test_support::MockRealmQuery;
    use std::rc::Rc;

    #[fuchsia::test]
    async fn test_get_dir_proxy_selector_bad_component() {
        let fake_realm_query = Rc::new(MockRealmQuery::default());
        let proxy = Rc::clone(&fake_realm_query).get_proxy().await;
        let moniker = Moniker::try_from("bad/component").unwrap();
        assert_matches!(get_dir_proxy(&moniker, &proxy).await, Err(_));
    }

    #[fuchsia::test]
    async fn test_get_dir_proxy_ok() {
        let fake_realm_query = Rc::new(MockRealmQuery::default());
        let proxy = Rc::clone(&fake_realm_query).get_proxy().await;
        let moniker = Moniker::try_from("example/component").unwrap();
        assert_matches!(get_dir_proxy(&moniker, &proxy).await, Ok(_));
    }
}
