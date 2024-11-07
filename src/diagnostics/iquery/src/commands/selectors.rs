// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::commands::types::*;
use crate::commands::utils;
use crate::types::Error;
use argh::{ArgsInfo, FromArgs};
use diagnostics_data::{Inspect, InspectData};
use diagnostics_hierarchy::DiagnosticsHierarchy;
use serde::Serialize;
use std::fmt;

/// Lists all available full selectors (component selector + tree selector).
/// If a selector is provided, it’ll only print selectors for that component.
/// If a full selector (component + tree) is provided, it lists all selectors under the given node.
#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "selectors")]
pub struct SelectorsCommand {
    #[argh(positional)]
    /// selectors for which the selectors should be queried. Minimum: 1 unless `--component` is set.
    /// When `--component` is provided then the selectors should be tree selectors, otherwise
    /// they can be component selectors or full selectors.
    pub selectors: Vec<String>,

    #[argh(option)]
    /// DEPRECATED: use `--component` instead.
    pub manifest: Option<String>,

    #[argh(option)]
    /// a fuzzy-search query. May include URL, moniker, or manifest fragments. No selector-escaping
    /// for moniker is needed in this query. Selectors following --component should omit the
    /// component selector, as they will be spliced together by the tool with the correct escaping.
    pub component: Option<String>,

    #[argh(option)]
    /// A string specifying what `fuchsia.diagnostics.ArchiveAccessor` to connect to.
    /// This can be copied from the output of `ffx inspect list-accessors`.
    /// The selector will be in the form of:
    /// <moniker>:<directory>:fuchsia.diagnostics.ArchiveAccessorName
    pub accessor: Option<String>,
}

impl Command for SelectorsCommand {
    type Result = SelectorsResult;

    async fn execute<P: DiagnosticsProvider>(self, provider: &P) -> Result<Self::Result, Error> {
        if self.manifest.is_some() {
            eprintln!(
                "WARNING: option `--manifest` is deprecated, please use `--component` instead"
            );
        }

        if self.selectors.is_empty() && self.component.is_none() && self.manifest.is_none() {
            return Err(Error::invalid_arguments("Expected 1 or more selectors. Got zero."));
        }

        let selectors = if let Some(component) = self.component {
            utils::process_component_query_with_partial_selectors(
                component,
                self.selectors.into_iter(),
                provider,
            )
            .await?
        } else if let Some(manifest) = self.manifest {
            utils::get_selectors_for_manifest(manifest, self.selectors, &self.accessor, provider)
                .await?
        } else {
            utils::process_fuzzy_inputs(self.selectors, provider).await?
        };

        let selectors = utils::expand_selectors(selectors, None)?;

        let mut results =
            provider.snapshot::<Inspect>(&self.accessor, selectors.into_iter()).await?;
        for result in results.iter_mut() {
            if let Some(hierarchy) = &mut result.payload {
                hierarchy.sort();
            }
        }
        Ok(SelectorsResult(inspect_to_selectors(results)))
    }
}

#[derive(Serialize)]
pub struct SelectorsResult(Vec<String>);

impl fmt::Display for SelectorsResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in self.0.iter() {
            writeln!(f, "{}", item)?;
        }
        Ok(())
    }
}

fn get_selectors(moniker: String, hierarchy: DiagnosticsHierarchy) -> Vec<String> {
    let component_selector = selectors::sanitize_moniker_for_selectors(&moniker);
    hierarchy
        .property_iter()
        .flat_map(|(node_path, maybe_property)| maybe_property.map(|prop| (node_path, prop)))
        .map(|(node_path, property)| {
            let node_selector = node_path
                .iter()
                .map(|s| selectors::sanitize_string_for_selectors(s))
                .collect::<Vec<_>>()
                .join("/");
            let property_selector = selectors::sanitize_string_for_selectors(property.name());
            format!("{}:{}:{}", component_selector, node_selector, property_selector)
        })
        .collect()
}

fn inspect_to_selectors(inspect_data: Vec<InspectData>) -> Vec<String> {
    let mut result = inspect_data
        .into_iter()
        .filter_map(|schema| {
            let moniker = schema.moniker;
            schema.payload.map(|hierarchy| get_selectors(moniker.to_string(), hierarchy))
        })
        .flatten()
        .collect::<Vec<_>>();
    result.sort();
    result
}
