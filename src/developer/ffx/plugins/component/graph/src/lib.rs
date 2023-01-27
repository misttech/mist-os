// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    component_debug::{graph::create_dot_graph, list::get_all_instances},
    ffx_component::rcs::{connect_to_realm_explorer, connect_to_realm_query},
    ffx_component_graph_args::ComponentGraphCommand,
    ffx_core::ffx_plugin,
    fidl_fuchsia_developer_remotecontrol as rc,
};

#[ffx_plugin()]
pub async fn graph(rcs_proxy: rc::RemoteControlProxy, cmd: ComponentGraphCommand) -> Result<()> {
    let query_proxy = connect_to_realm_query(&rcs_proxy).await?;
    let explorer_proxy = connect_to_realm_explorer(&rcs_proxy).await?;

    let instances = get_all_instances(&explorer_proxy, &query_proxy, cmd.filter).await?;

    let output = create_dot_graph(instances, cmd.orientation);
    println!("{}", output);
    Ok(())
}
