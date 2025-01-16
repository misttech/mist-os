// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::{ffx_bail, ffx_error};
use fuchsia_url::AbsoluteComponentUrl;

pub mod format;
pub mod rcs;

/// Parses a string into an absolute component URL.
pub fn parse_component_url(url: &str) -> Result<AbsoluteComponentUrl> {
    let url = match AbsoluteComponentUrl::parse(url) {
        Ok(url) => url,
        Err(e) => ffx_bail!("URL parsing error: {:?}", e),
    };

    let manifest = url
        .resource()
        .split('/')
        .next_back()
        .ok_or(ffx_error!("Could not extract manifest filename from URL"))?;

    if let Some(_) = manifest.strip_suffix(".cm") {
        Ok(url)
    } else {
        ffx_bail!(
            "{} is not a component manifest! Component manifests must end in the `cm` extension.",
            manifest
        )
    }
}
