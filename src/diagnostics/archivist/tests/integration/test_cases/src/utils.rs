// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error, Result};
use diagnostics_reader::{ArchiveReader, Logs};
use fidl_fuchsia_diagnostics::{self as fdiagnostics, ArchiveAccessorMarker, Interest, Severity};
use realm_proxy_client::RealmProxyClient;
use selectors::{parse_component_selector, VerboseError};

/// Returns a snapshot of the realm's logs as a stream.
///
/// The realm must expose fuchsia.diagnostics.ArchiveAccessor.
pub(crate) async fn snapshot_and_stream_logs(
    realm_proxy: &RealmProxyClient,
) -> impl crate::assert::LogStream {
    let accessor = realm_proxy
        .connect_to_protocol::<ArchiveAccessorMarker>()
        .await
        .expect("connect to archive accessor");

    let subscription = ArchiveReader::new()
        .with_archive(accessor)
        .snapshot_then_subscribe::<Logs>()
        .expect("subscribe to logs");
    subscription.wait_for_ready().await;
    subscription
}

/// Extension methods on LogSettingsProxy.
#[async_trait::async_trait]
pub(crate) trait LogSettingsExt {
    /// Changes the logs interest configuration for a set of components that match `selector`
    ///
    /// # Errors
    ///
    /// Returns an error if `selector` is not a valid component selector.
    /// Returns an error if the call to LogSettings/SetInterest fails.
    async fn set_component_interest(&self, selector: &str, severity: Severity)
        -> Result<(), Error>;
}

#[async_trait::async_trait]
impl LogSettingsExt for fdiagnostics::LogSettingsProxy {
    async fn set_component_interest(
        &self,
        selector: &str,
        severity: Severity,
    ) -> Result<(), Error> {
        let component_selector = parse_component_selector::<VerboseError>(selector)?;
        let interests = [fdiagnostics::LogInterestSelector {
            selector: component_selector,
            interest: Interest { min_severity: Some(severity), ..Default::default() },
        }];
        self.set_interest(&interests).await.context("set interest")?;
        Ok(())
    }
}
