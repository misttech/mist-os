// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{commands::types::*, types::Error},
    argh::FromArgs,
    async_trait::async_trait,
    diagnostics_data::{Inspect, InspectData},
    serde::{Serialize, Serializer},
    std::{cmp::Ordering, collections::BTreeSet, fmt},
};

#[derive(Debug, Eq, PartialEq, PartialOrd, Ord, Serialize)]
pub struct MonikerWithUrl {
    pub moniker: String,
    pub component_url: String,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ListResultItem {
    Moniker(String),
    MonikerWithUrl(MonikerWithUrl),
}

impl ListResultItem {
    pub fn into_moniker(self) -> String {
        match self {
            Self::Moniker(moniker) => moniker,
            Self::MonikerWithUrl(MonikerWithUrl { moniker, .. }) => moniker,
        }
    }
}

impl Ord for ListResultItem {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (ListResultItem::Moniker(moniker), ListResultItem::Moniker(other_moniker))
            | (
                ListResultItem::MonikerWithUrl(MonikerWithUrl { moniker, .. }),
                ListResultItem::MonikerWithUrl(MonikerWithUrl { moniker: other_moniker, .. }),
            ) => moniker.cmp(other_moniker),
            _ => unreachable!("all lists must contain variants of the same type"),
        }
    }
}

impl PartialOrd for ListResultItem {
    // Compare based on the moniker only. To enable sorting using the moniker only.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Serialize for ListResultItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Moniker(string) => serializer.serialize_str(&string),
            Self::MonikerWithUrl(data) => data.serialize(serializer),
        }
    }
}

#[derive(Serialize)]
pub struct ListResult(Vec<ListResultItem>);

impl ListResult {
    pub(crate) fn into_inner(self) -> Vec<ListResultItem> {
        self.0
    }
}

impl fmt::Display for ListResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in self.0.iter() {
            match item {
                ListResultItem::Moniker(moniker) => writeln!(f, "{}", moniker)?,
                ListResultItem::MonikerWithUrl(MonikerWithUrl { component_url, moniker }) => {
                    writeln!(f, "{}:", moniker)?;
                    writeln!(f, "  {}", component_url)?;
                }
            }
        }
        Ok(())
    }
}

fn components_from_inspect_data(inspect_data: Vec<InspectData>) -> Vec<ListResultItem> {
    let mut result = vec![];
    for value in inspect_data {
        match value.metadata.component_url {
            Some(ref url) => {
                result.push(ListResultItem::MonikerWithUrl(MonikerWithUrl {
                    moniker: value.moniker,
                    component_url: url.clone(),
                }));
            }
            None => {
                result.push(ListResultItem::Moniker(value.moniker));
            }
        }
    }
    result
}

pub fn list_response_items_from_components(
    manifest: &Option<String>,
    with_url: bool,
    components: Vec<ListResultItem>,
) -> Vec<ListResultItem> {
    components
        .into_iter()
        .filter(|result| match manifest {
            None => true,
            Some(manifest) => match result {
                ListResultItem::Moniker(_) => true,
                ListResultItem::MonikerWithUrl(url) => url.component_url.contains(manifest),
            },
        })
        .map(|result| {
            if with_url {
                result
            } else {
                match result {
                    ListResultItem::Moniker(_) => result,
                    ListResultItem::MonikerWithUrl(val) => ListResultItem::Moniker(val.moniker),
                }
            }
        })
        // Collect as btreeset to sort and remove potential duplicates.
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
}

/// Lists all components (relative to the scope where the archivist receives events from) of
/// components that expose inspect.
#[derive(Default, FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "list")]
pub struct ListCommand {
    #[argh(option)]
    /// the name of the manifest file that we are interested in. If this is provided, the output
    /// will only contain monikers for components whose url contains the provided name.
    pub manifest: Option<String>,

    #[argh(switch)]
    /// also print the URL of the component.
    pub with_url: bool,

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
impl Command for ListCommand {
    type Result = ListResult;

    async fn execute<P: DiagnosticsProvider>(&self, provider: &P) -> Result<Self::Result, Error> {
        let inspect = provider.snapshot::<Inspect>(&self.accessor, &[]).await?;
        let components = components_from_inspect_data(inspect);
        let results =
            list_response_items_from_components(&self.manifest, self.with_url, components);
        Ok(ListResult(results))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_data::{InspectHandleName, Timestamp};

    #[fuchsia::test]
    fn components_from_inspect_data_uses_diagnostics_ready() {
        let inspect_data = vec![
            InspectData::for_inspect(
                "some_moniker",
                /*inspect_hierarchy=*/ None,
                Timestamp::from(123456789800i64),
                "fake-url",
                Some(InspectHandleName::filename("fake-file")),
                vec![],
            ),
            InspectData::for_inspect(
                "other_moniker",
                /*inspect_hierarchy=*/ None,
                Timestamp::from(123456789900i64),
                "other-fake-url",
                Some(InspectHandleName::filename("fake-file")),
                vec![],
            ),
            InspectData::for_inspect(
                "some_moniker",
                /*inspect_hierarchy=*/ None,
                Timestamp::from(123456789910i64),
                "fake-url",
                Some(InspectHandleName::filename("fake-file")),
                vec![],
            ),
            InspectData::for_inspect(
                "different_moniker",
                /*inspect_hierarchy=*/ None,
                Timestamp::from(123456790990i64),
                "different-fake-url",
                Some(InspectHandleName::filename("fake-file")),
                vec![],
            ),
        ];

        let components = components_from_inspect_data(inspect_data);

        assert_eq!(components.len(), 4);
    }
}
