// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::capability::{find_instances_that_expose_or_use_capability, MatchingInstances},
    anyhow::Result,
    fidl_fuchsia_sys2 as fsys,
};

pub async fn capability_cmd<W: std::io::Write>(
    capability_name: String,
    realm_query: fsys::RealmQueryProxy,
    realm_explorer: fsys::RealmExplorerProxy,
    mut writer: W,
) -> Result<()> {
    let MatchingInstances { exposed, used } = find_instances_that_expose_or_use_capability(
        capability_name,
        &realm_explorer,
        &realm_query,
    )
    .await?;

    if !exposed.is_empty() {
        writeln!(writer, "Exposed:")?;
        for component in exposed {
            writeln!(writer, "  {}", component)?;
        }
    }
    if !used.is_empty() {
        writeln!(writer, "Used:")?;
        for component in used {
            writeln!(writer, "  {}", component)?;
        }
    }

    Ok(())
}
