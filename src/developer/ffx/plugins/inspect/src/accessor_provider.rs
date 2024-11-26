// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use diagnostics_data::Data;
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_diagnostics::ClientSelectorConfiguration::{SelectAll, Selectors};
use fidl_fuchsia_diagnostics::{
    Format, Selector, SelectorArgument, StreamParameters, StringSelector, TreeSelector,
};
use fidl_fuchsia_diagnostics_host::{ArchiveAccessorMarker, ArchiveAccessorProxy};
use fidl_fuchsia_io::OpenFlags;
use fidl_fuchsia_sys2 as fsys2;
use futures::AsyncReadExt;
use iquery::commands::{get_accessor_selectors, DiagnosticsProvider};
use iquery::types::Error;
use serde::Deserialize;
use std::borrow::Cow;

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

pub struct HostArchiveReader {
    diagnostics_proxy: ArchiveAccessorProxy,
    rcs_proxy: RemoteControlProxy,
}

fn add_host_before_last_dot(input: &str) -> Result<String, Error> {
    let (rest, last) = input.rsplit_once('.').ok_or(Error::NotEnoughDots)?;
    Ok(format!("{}.host.{}", rest, last))
}

struct MonikerAndProtocol {
    protocol: String,
    moniker: String,
}

impl TryFrom<Selector> for MonikerAndProtocol {
    type Error = Error;

    fn try_from(selector: Selector) -> Result<Self, Self::Error> {
        Ok(MonikerAndProtocol {
            moniker: selector
                .component_selector
                .map(|selector| selector.moniker_segments)
                .flatten()
                .into_iter()
                .flatten()
                .map(|value| match value {
                    StringSelector::ExactMatch(value) => Ok(value),
                    _ => Err(Error::MustBeExactMoniker),
                })
                .collect::<Result<Vec<_>, Error>>()?
                .join("/"),
            protocol: selector
                .tree_selector
                .map(|value| match value {
                    TreeSelector::PropertySelector(value) => Ok(value.target_properties),
                    _ => Err(Error::MustUsePropertySelector),
                })
                .into_iter()
                .flatten()
                .map(|value| match value {
                    StringSelector::ExactMatch(value) => Ok(value),
                    _ => Err(Error::MustBeExactProtocol),
                })
                .next()
                .ok_or(Error::MustBeExactProtocol)??,
        })
    }
}

impl HostArchiveReader {
    pub fn new(diagnostics_proxy: ArchiveAccessorProxy, rcs_proxy: RemoteControlProxy) -> Self {
        Self { diagnostics_proxy, rcs_proxy }
    }

    pub async fn snapshot_diagnostics_data<D>(
        &self,
        accessor: &Option<String>,
        selectors: impl IntoIterator<Item = Selector>,
    ) -> Result<Vec<Data<D>>, Error>
    where
        D: diagnostics_data::DiagnosticsData,
    {
        let mut selectors = selectors.into_iter().peekable();
        let selectors = if selectors.peek().is_none() {
            SelectAll(true)
        } else {
            Selectors(selectors.map(|s| SelectorArgument::StructuredSelector(s)).collect())
        };

        let accessor = match accessor {
            Some(ref s) => {
                let s = add_host_before_last_dot(s)?;
                let selector = selectors::parse_verbose(&s).map_err(|e| {
                    Error::ParseSelector("unable to parse selector".to_owned(), anyhow!("{:?}", e))
                })?;
                let moniker_and_protocol = MonikerAndProtocol::try_from(selector)?;

                let (client, server) = fidl::endpoints::create_endpoints::<ArchiveAccessorMarker>();
                self.rcs_proxy
                    .deprecated_open_capability(
                        &format!("/{}", moniker_and_protocol.moniker),
                        fsys2::OpenDirType::ExposedDir,
                        &moniker_and_protocol.protocol,
                        server.into_channel(),
                        OpenFlags::empty(),
                    )
                    .await??;
                Cow::Owned(client.into_proxy()?)
            }
            None => Cow::Borrowed(&self.diagnostics_proxy),
        };

        let params = StreamParameters {
            stream_mode: Some(fidl_fuchsia_diagnostics::StreamMode::Snapshot),
            data_type: Some(D::DATA_TYPE),
            format: Some(Format::Json),
            client_selector_configuration: Some(selectors),
            ..Default::default()
        };

        let (client, server) = fuchsia_async::emulated_handle::Socket::create_stream();

        let _ = accessor.stream_diagnostics(&params, server).await.map_err(|s| {
            Error::IOError(
                "call diagnostics_proxy".into(),
                anyhow!("failure setting up diagnostics stream: {:?}", s),
            )
        })?;

        let mut client = fuchsia_async::Socket::from_socket(client);

        let mut output = vec![];
        match client.read_to_end(&mut output).await {
            Err(e) => Err(Error::IOError("get next".into(), e.into())),
            Ok(_) => Ok(serde_json::Deserializer::from_slice(&output)
                .into_iter::<OneOrMany<Data<D>>>()
                .filter_map(|value| value.ok())
                .map(|value| match value {
                    OneOrMany::One(value) => vec![value],
                    OneOrMany::Many(values) => values,
                })
                .flatten()
                .collect()),
        }
    }
}

impl DiagnosticsProvider for HostArchiveReader {
    async fn snapshot<D>(
        &self,
        accessor_path: &Option<String>,
        selectors: impl IntoIterator<Item = Selector>,
    ) -> Result<Vec<Data<D>>, Error>
    where
        D: diagnostics_data::DiagnosticsData,
    {
        self.snapshot_diagnostics_data::<D>(accessor_path, selectors).await
    }

    async fn get_accessor_paths(&self) -> Result<Vec<String>, Error> {
        let query_proxy = self.connect_realm_query().await?;
        get_accessor_selectors(&query_proxy).await
    }

    async fn connect_realm_query(&self) -> Result<fsys2::RealmQueryProxy, Error> {
        rcs::root_realm_query(&self.rcs_proxy, std::time::Duration::from_secs(15))
            .await
            .map_err(|e| Error::ConnectToProtocol("RemoteControlProxy (host)".to_string(), e))
    }
}
