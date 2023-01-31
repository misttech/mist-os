// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::list::{create_table, get_all_instances, Instance, ListFilter},
    anyhow::Result,
    fidl_fuchsia_sys2 as fsys,
};

pub async fn list_cmd_print<W: std::io::Write>(
    filter: Option<ListFilter>,
    verbose: bool,
    realm_query: fsys::RealmQueryProxy,
    realm_explorer: fsys::RealmExplorerProxy,
    mut writer: W,
) -> Result<()> {
    let instances = get_all_instances(&realm_explorer, &realm_query, filter).await?;

    if verbose {
        let table = create_table(instances);
        table.print(&mut writer)?;
    } else {
        for instance in instances {
            writeln!(writer, "{}", instance.moniker)?;
        }
    }

    Ok(())
}

pub async fn list_cmd_serialized(
    filter: Option<ListFilter>,
    realm_query: fsys::RealmQueryProxy,
    realm_explorer: fsys::RealmExplorerProxy,
) -> Result<Vec<Instance>> {
    get_all_instances(&realm_explorer, &realm_query, filter).await
}
