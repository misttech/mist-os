// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use cm_rust::{CapabilityDecl, CapabilityDeclCommon};
use component_debug::capability::{get_all_route_segments, RouteSegment};
use errors::{ffx_bail, ffx_error};
use fidl::{endpoints::DiscoverableProtocolMarker, Status};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_diagnostics::Selector;
use fidl_fuchsia_memory_heapdump_client as fheapdump_client;
use fidl_fuchsia_sys2::RealmQueryProxy;

const COLLECTOR_CAPABILITY: &str = fheapdump_client::CollectorMarker::PROTOCOL_NAME;

/// Retrieve the monikers of all the collectors in the system, i.e. all the components that declare
/// the COLLECTOR_CAPABILITY.
async fn list_collectors(query_proxy: &RealmQueryProxy) -> anyhow::Result<Vec<String>> {
    Ok(get_all_route_segments(COLLECTOR_CAPABILITY.to_string(), query_proxy)
        .await?
        .into_iter()
        .filter_map(|rs| match rs {
            RouteSegment::DeclareBy { moniker, capability: CapabilityDecl::Protocol(protocol) }
                if protocol.name() == COLLECTOR_CAPABILITY =>
            {
                Some(
                    moniker
                        .to_string()
                        .strip_prefix('/')
                        .expect("should start with '/'")
                        .to_string(),
                )
            }
            _ => None,
        })
        .collect())
}

/// Given a collector's moniker, create a selector for its COLLECTOR_CAPABILITY and parse it.
fn build_collector_selector(moniker: &str) -> anyhow::Result<(Selector, String)> {
    let selector_str = format!(
        "{}:expose:{}",
        selectors::sanitize_moniker_for_selectors(moniker),
        COLLECTOR_CAPABILITY
    );
    let selector = selectors::parse_selector::<selectors::VerboseError>(&selector_str)
        .map_err(|err| ffx_error!("Failed to parse selector {}: {}", selector_str, err))?;
    Ok((selector, selector_str))
}

/// Connects to the collector.
///
/// If a moniker is provided, this function directly connects to it.
///
/// If no moniker is provided, this function checks if there is exactly one collector component in
/// the system and connects to it.
pub async fn connect_to_collector(
    remote_control: &RemoteControlProxy,
    moniker: Option<String>,
) -> anyhow::Result<fheapdump_client::CollectorProxy> {
    let (query_proxy, query_server) = fidl::endpoints::create_proxy()?;
    remote_control.root_realm_query(query_server).await?.map_err(Status::from_raw)?;

    let moniker = if let Some(moniker) = moniker {
        moniker
    } else {
        let candidates = list_collectors(&query_proxy).await?;
        if candidates.len() > 1 {
            ffx_bail!(
                "More than one collector was found, use --collector to disambiguate:\n{}",
                candidates.join("\n")
            );
        } else if let Some(candidate) = candidates.into_iter().next() {
            candidate
        } else {
            ffx_bail!("No collector found");
        }
    };

    let (selector, selector_str) = build_collector_selector(&moniker)?;
    let (collector_proxy, collector_server) =
        fidl::endpoints::create_proxy::<fheapdump_client::CollectorMarker>()?;
    remote_control.connect(selector, collector_server.into_channel()).await?.map_err(|err| {
        ffx_error!(
            "Attempting to connect to moniker {} (full selector {}) failed with {:?}",
            moniker,
            selector_str,
            err
        )
    })?;

    Ok(collector_proxy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_diagnostics::{PropertySelector, StringSelector::ExactMatch, TreeSelector};
    use test_case::test_case;

    // Verifies that build_collector_selector is not confused by the presence of collections, whose
    // ':' character needs to be internally escaped.
    #[test_case("core/foo/bar" ; "no collections")]
    #[test_case("core/ffx-laboratory:foo/heapdump-collector" ; "parent is in a collection")]
    #[test_case("core/ffx-laboratory/foo:bar" ; "leaf is in a collection")]
    fn test_build_collector_selector(moniker: &str) {
        let (selector, _) = build_collector_selector(moniker).unwrap();

        // The moniker must have been split at '/' without any special handling for ':'
        let expected_segments: Vec<_> =
            moniker.split('/').map(|segment| ExactMatch(segment.to_string())).collect();
        assert_eq!(
            selector.component_selector.unwrap().moniker_segments.unwrap(),
            expected_segments
        );

        // The tree selector should always point to the exposed COLLECTOR_CAPABILITY
        assert_eq!(
            selector.tree_selector.unwrap(),
            TreeSelector::PropertySelector(PropertySelector {
                node_path: vec![ExactMatch("expose".to_string())],
                target_properties: ExactMatch(COLLECTOR_CAPABILITY.to_string()),
            })
        );
    }
}
