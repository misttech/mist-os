// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::commands::types::*;
use crate::commands::utils;
use crate::text_formatter;
use crate::types::Error;
use argh::{ArgsInfo, FromArgs};
use derivative::Derivative;
use diagnostics_data::{Inspect, InspectData};
use serde::Serialize;
use std::cmp::Ordering;
use std::fmt;
use std::ops::Deref;

#[derive(Derivative, Serialize, PartialEq)]
#[derivative(Eq)]
pub struct ShowResultItem(InspectData);

impl Deref for ShowResultItem {
    type Target = InspectData;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialOrd for ShowResultItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ShowResultItem {
    fn cmp(&self, other: &Self) -> Ordering {
        self.moniker.cmp(&other.moniker).then_with(|| {
            let this_name = self.metadata.name.as_ref();
            let other_name = other.metadata.name.as_ref();
            this_name.cmp(other_name)
        })
    }
}

#[derive(Serialize)]
pub struct ShowResult(Vec<ShowResultItem>);

impl fmt::Display for ShowResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in self.0.iter() {
            text_formatter::output_schema(f, &item.0)?;
        }
        Ok(())
    }
}

/// Prints the inspect hierarchies that match the given selectors.
/// See https://fuchsia.dev/fuchsia-src/development/diagnostics/inspect#userspace_tools for more.
#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "show")]
pub struct ShowCommand {
    #[argh(positional)]
    /// queries for accessing Inspect data.
    /// If no selectors are provided, Inspect data for the whole system will be returned.
    pub selectors: Vec<String>,

    #[argh(option)]
    /// a fuzzy-search query. May include URL, moniker, or manifest fragments. No selector-escaping
    /// for moniker is needed in this query. Selectors following --component should omit the
    /// component selector, as they will be spliced together by the tool with the correct escaping.
    pub component: Option<String>,

    #[argh(option)]
    /// A string specifying what `fuchsia.diagnostics.ArchiveAccessor` to connect to.
    /// This can be copied from the output of `ffx inspect list-accessors`.
    /// The selector will be in the form of:
    /// <moniker>:fuchsia.diagnostics.ArchiveAccessorName
    pub accessor: Option<String>,

    #[argh(option)]
    /// specifies a tree published by a component by name.
    ///
    /// If a selector is also provided, the specified name will be added to the selector.
    pub name: Option<String>,
}

impl Command for ShowCommand {
    type Result = ShowResult;

    async fn execute<P: DiagnosticsProvider>(self, provider: &P) -> Result<Self::Result, Error> {
        let mut selectors = if let Some(component) = self.component {
            utils::process_component_query_with_partial_selectors(
                component,
                self.selectors.into_iter(),
                provider,
            )
            .await?
        } else {
            utils::process_fuzzy_inputs(self.selectors, provider).await?
        };

        utils::ensure_tree_field_is_set(&mut selectors, self.name)?;
        let inspect_data_iter =
            provider.snapshot::<Inspect>(self.accessor.as_deref(), selectors).await?.into_iter();

        let mut results = inspect_data_iter
            .map(|mut d: InspectData| {
                if let Some(hierarchy) = &mut d.payload {
                    hierarchy.sort();
                }
                ShowResultItem(d)
            })
            .collect::<Vec<_>>();

        results.sort();
        Ok(ShowResult(results))
    }
}
