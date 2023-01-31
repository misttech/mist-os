// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        graph::{create_dot_graph, GraphOrientation},
        list::{get_all_instances, ListFilter},
    },
    anyhow::Result,
    fidl_fuchsia_sys2 as fsys,
};

pub async fn graph_cmd<W: std::io::Write>(
    filter: Option<ListFilter>,
    orientation: GraphOrientation,
    realm_query: fsys::RealmQueryProxy,
    realm_explorer: fsys::RealmExplorerProxy,
    mut writer: W,
) -> Result<()> {
    let instances = get_all_instances(&realm_explorer, &realm_query, filter).await?;

    let output = create_dot_graph(instances, orientation);
    writeln!(writer, "{}", output)?;

    Ok(())
}
