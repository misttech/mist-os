// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use diagnostics_data::Data;
use fidl_fuchsia_diagnostics::ClientSelectorConfiguration::{SelectAll, Selectors};
use fidl_fuchsia_diagnostics::{Format, Selector, SelectorArgument, StreamParameters};
use fidl_fuchsia_diagnostics_host::{ArchiveAccessorMarker, ArchiveAccessorProxy};
use fidl_fuchsia_sys2 as fsys2;
use futures::AsyncReadExt;
use iquery::commands::{connect_accessor, get_accessor_selectors, DiagnosticsProvider};
use iquery::types::Error;
use moniker::Moniker;
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
    query_proxy: fsys2::RealmQueryProxy,
}

fn add_host_before_last_dot(input: &str) -> Result<String, Error> {
    let (rest, last) = input.rsplit_once('.').ok_or(Error::NotEnoughDots)?;
    Ok(format!("{}.host.{}", rest, last))
}

fn moniker_and_protocol(s: &str) -> Result<(Moniker, &str), Error> {
    let (moniker, protocol) = s.rsplit_once(":").ok_or_else(|| Error::invalid_accessor(s))?;
    let moniker = Moniker::try_from(moniker).map_err(|_| Error::invalid_accessor(s))?;
    Ok((moniker, protocol))
}

impl HostArchiveReader {
    pub fn new(
        diagnostics_proxy: ArchiveAccessorProxy,
        query_proxy: fsys2::RealmQueryProxy,
    ) -> Self {
        Self { diagnostics_proxy, query_proxy }
    }

    pub async fn snapshot_diagnostics_data<D>(
        &self,
        accessor: Option<&str>,
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

        let accessor = match accessor.as_deref() {
            Some(s) => {
                let s = add_host_before_last_dot(s)?;
                let (moniker, protocol) = moniker_and_protocol(s.as_str())?;
                let proxy = connect_accessor::<ArchiveAccessorMarker>(
                    &moniker,
                    protocol,
                    &self.query_proxy,
                )
                .await?;
                Cow::Owned(proxy)
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
                "call ArchiveAccessor".into(),
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
        accessor_path: Option<&str>,
        selectors: impl IntoIterator<Item = Selector>,
    ) -> Result<Vec<Data<D>>, Error>
    where
        D: diagnostics_data::DiagnosticsData,
    {
        self.snapshot_diagnostics_data::<D>(accessor_path, selectors).await
    }

    async fn get_accessor_paths(&self) -> Result<Vec<String>, Error> {
        get_accessor_selectors(&self.query_proxy).await
    }

    fn realm_query(&self) -> &fsys2::RealmQueryProxy {
        &self.query_proxy
    }
}
