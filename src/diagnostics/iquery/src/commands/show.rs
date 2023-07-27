// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        commands::{types::*, utils},
        text_formatter,
        types::Error,
    },
    argh::FromArgs,
    async_trait::async_trait,
    derivative::Derivative,
    diagnostics_data::{Inspect, InspectData, InspectHandleName},
    glob,
    serde::Serialize,
    std::{cmp::Ordering, fmt, ops::Deref},
};

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
            let this_name = self.metadata.name.as_ref().map(InspectHandleName::as_ref);
            let other_name = other.metadata.name.as_ref().map(InspectHandleName::as_ref);
            this_name.cmp(&other_name)
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
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "show")]
pub struct ShowCommand {
    #[argh(option)]
    /// the name of the manifest file that we are interested in. If this is provided, the output
    /// will only contain monikers for components whose url contains the provided name.
    pub manifest: Option<String>,

    #[argh(positional)]
    /// selectors for which the selectors should be queried. If no selectors are provided, inspect
    /// data for the whole system will be returned. If `--manifest` is provided then the selectors
    /// should be tree selectors, otherwise component selectors or full selectors.
    pub selectors: Vec<String>,

    #[argh(option)]
    /// the filenames we are interested in. If any are provided, the output will only
    /// contain data from components which expose Inspect under the given file under
    /// their out/diagnostics directory.
    /// Supports shell globs expansions.
    pub file: Vec<String>,

    #[argh(option)]
    /// A selector specifying what `fuchsia.diagnostics.ArchiveAccessor` to connect to.
    /// The selector will be in the form of:
    /// <moniker>:<directory>:fuchsia.diagnostics.ArchiveAccessorName
    ///
    /// Typically this is the output of `iquery list-accessors`.
    ///
    /// For example: `bootstrap/archivist:expose:fuchsia.diagnostics.FeedbackArchiveAccessor`
    /// means that the command will connect to the `FeedbackArchiveAccecssor`
    /// exposed by `bootstrap/archivist`.
    pub accessor: Option<String>,
}

#[async_trait]
impl Command for ShowCommand {
    type Result = ShowResult;

    async fn execute<P: DiagnosticsProvider>(&self, provider: &P) -> Result<Self::Result, Error> {
        let selectors = utils::get_selectors_for_manifest(
            &self.manifest,
            &self.selectors,
            &self.accessor,
            provider,
        )
        .await?;
        let selectors = utils::expand_selectors(selectors)?;

        let inspect_data_iter =
            provider.snapshot::<Inspect>(&self.accessor, &selectors).await?.into_iter();
        // Filter out by filename on the Inspect metadata.
        let filter_fn: Box<dyn Fn(&InspectData) -> bool> = if !&self.file.is_empty() {
            let mut glob_patterns = vec![];
            for file in self.file.iter() {
                glob_patterns.push(
                    glob::Pattern::new(&file)
                        .map_err(|_e| Error::InvalidFilePattern(file.to_owned()))?,
                )
            }
            Box::new(move |d: &InspectData| {
                glob_patterns.iter().any(|glob| {
                    glob.matches(&d.metadata.name.as_ref().map(|name| name.as_ref()).unwrap_or(""))
                })
            })
        } else {
            Box::new(|_d: &InspectData| true)
        };

        let mut results = inspect_data_iter
            .filter(filter_fn)
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
