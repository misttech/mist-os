// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod capability;
pub mod create;
pub mod destroy;
pub mod doctor;
pub mod explore;
pub mod graph;
pub mod list;
pub mod reload;
pub mod resolve;
pub mod run;
pub mod show;
pub mod start;
pub mod stop;

mod format;

pub use {
    capability::capability_cmd,
    create::create_cmd,
    destroy::destroy_cmd,
    doctor::{doctor_cmd_print, doctor_cmd_serialized},
    explore::explore_cmd,
    graph::graph_cmd,
    list::{list_cmd_print, list_cmd_serialized},
    reload::reload_cmd,
    resolve::resolve_cmd,
    run::run_cmd,
    show::{show_cmd_print, show_cmd_serialized},
    start::start_cmd,
    stop::stop_cmd,
};

use {
    anyhow::{bail, format_err, Result},
    fuchsia_url::AbsoluteComponentUrl,
};

/// Parses a string into an absolute component URL.
pub(crate) fn parse_component_url(url: &str) -> Result<AbsoluteComponentUrl> {
    let url = match AbsoluteComponentUrl::parse(url) {
        Ok(url) => url,
        Err(e) => bail!("URL parsing error: {:?}", e),
    };

    let manifest = url
        .resource()
        .split('/')
        .last()
        .ok_or(format_err!("Could not extract manifest filename from URL"))?;

    if let Some(_) = manifest.strip_suffix(".cm") {
        Ok(url)
    } else {
        bail!(
            "{} is not a component manifest! Component manifests must end in the `cm` extension.",
            manifest
        )
    }
}
