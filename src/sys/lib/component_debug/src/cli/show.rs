// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::show::{create_table, find_instances, Instance},
    anyhow::{bail, Result},
    fidl_fuchsia_sys2 as fsys,
};

pub async fn show_cmd_print<W: std::io::Write>(
    query: String,
    realm_query: fsys::RealmQueryProxy,
    realm_explorer: fsys::RealmExplorerProxy,
    mut writer: W,
) -> Result<()> {
    let instances = find_instances(query, &realm_explorer, &realm_query).await?;

    if instances.is_empty() {
        // TODO(fxbug.dev/104031): Clarify the exit code policy of this plugin.
        bail!("No matching components found");
    }

    for instance in instances {
        let table = create_table(instance);
        table.print(&mut writer)?;
        writeln!(&mut writer, "")?;
    }

    Ok(())
}

pub async fn show_cmd_serialized(
    query: String,
    realm_query: fsys::RealmQueryProxy,
    realm_explorer: fsys::RealmExplorerProxy,
) -> Result<Vec<Instance>> {
    find_instances(query, &realm_explorer, &realm_query).await
}
