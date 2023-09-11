// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {crate::capability::*, anyhow::Result, fidl_fuchsia_sys2 as fsys};

pub async fn capability_cmd<W: std::io::Write>(
    query: String,
    realm_query: fsys::RealmQueryProxy,
    mut writer: W,
) -> Result<()> {
    let segments = get_all_route_segments(query, &realm_query).await?;

    let mut decls = vec![];
    let mut exposes = vec![];
    let mut offers = vec![];
    let mut uses = vec![];

    for s in segments {
        match &s {
            RouteSegment::DeclareBy { .. } => decls.push(s),
            RouteSegment::ExposeBy { .. } => exposes.push(s),
            RouteSegment::OfferBy { .. } => offers.push(s),
            RouteSegment::UseBy { .. } => uses.push(s),
        }
    }

    if decls.is_empty() {
        writeln!(writer, "Declarations: None")?;
    } else {
        writeln!(writer, "Declarations:")?;
        for decl in decls {
            writeln!(writer, "  {}", decl)?;
        }
    }

    writeln!(writer, "")?;

    if exposes.is_empty() {
        writeln!(writer, "Exposes: None")?;
    } else {
        writeln!(writer, "Exposes:")?;
        for decl in exposes {
            writeln!(writer, "  {}", decl)?;
        }
    }

    writeln!(writer, "")?;

    if offers.is_empty() {
        writeln!(writer, "Offers: None")?;
    } else {
        writeln!(writer, "Offers:")?;
        for decl in offers {
            writeln!(writer, "  {}", decl)?;
        }
    }

    writeln!(writer, "")?;

    if uses.is_empty() {
        writeln!(writer, "Uses: None")?;
    } else {
        writeln!(writer, "Uses:")?;
        for decl in uses {
            writeln!(writer, "  {}", decl)?;
        }
    }

    Ok(())
}
