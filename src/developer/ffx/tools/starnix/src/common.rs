// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context, Result};
use component_debug::cli;
use fidl_fuchsia_developer_remotecontrol as rc;
use fidl_fuchsia_starnix_container::{ControllerMarker, ControllerProxy};
use lazy_static::lazy_static;
use regex::Regex;
use target_connector::Connector;
use target_holders::RemoteControlProxyHolder;

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Returns the moniker for the container in the session, if there is one.
async fn find_session_container(rcs_proxy: &rc::RemoteControlProxy) -> Result<String> {
    lazy_static! {
        // Example: core/session-manager/session:session/elements:5udqa81zlypamvgu/container
        static ref SESSION_CONTAINER: Regex =
            Regex::new(r"^core/session-manager/session:session/elements:\w+/container$")
                .unwrap();
    }

    let query_proxy =
        rcs::root_realm_query(&rcs_proxy, TIMEOUT).await.context("opening realm query")?;
    let instances = cli::list::get_instances_matching_filter(None, &query_proxy).await?;
    let containers: Vec<_> = instances
        .into_iter()
        .filter(|i| {
            let moniker = i.moniker.to_string();
            (*SESSION_CONTAINER).is_match(&moniker)
        })
        .collect();

    if containers.is_empty() {
        println!("Unable to find Starnix container in the session.");
        println!("Please specify a container with --moniker");
        bail!("cannot find container")
    }

    if containers.len() > 1 {
        println!("Found multiple Starnix containers in the session:");
        for container in containers.iter() {
            println!("  {}", container.moniker)
        }
        println!("Please specify a container with --moniker");
        bail!("too many containers")
    }

    Ok(containers[0].moniker.to_string())
}

async fn find_moniker(
    rcs_proxy: &rc::RemoteControlProxy,
    moniker: Option<String>,
) -> Result<String> {
    if let Some(moniker) = moniker {
        return Ok(moniker);
    }
    find_session_container(&rcs_proxy).await
}

pub async fn connect_to_rcs(
    rcs_connector: &Connector<RemoteControlProxyHolder>,
) -> Result<RemoteControlProxyHolder> {
    rcs_connector
        .try_connect(|target, _err| {
            eprintln!("Waiting for {target:?}...");
            Ok(())
        })
        .await
        .context("connecting to RCS")
}

pub async fn connect_to_contoller(
    rcs_proxy: &rc::RemoteControlProxy,
    moniker: Option<String>,
) -> Result<ControllerProxy> {
    let moniker = find_moniker(&rcs_proxy, moniker).await?;
    rcs::connect_to_protocol::<ControllerMarker>(TIMEOUT, &moniker, &rcs_proxy).await
}
