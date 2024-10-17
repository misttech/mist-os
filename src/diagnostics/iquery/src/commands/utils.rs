// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::commands::types::DiagnosticsProvider;
use crate::commands::{Command, ListCommand};
use crate::types::Error;
use cm_rust::SourceName;
use component_debug::realm::*;
use fidl_fuchsia_sys2 as fsys2;
use moniker::Moniker;
use regex::Regex;
use std::sync::LazyLock;

static EXPECTED_PROTOCOL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r".*fuchsia\.diagnostics\..*ArchiveAccessor$").unwrap());

/// Returns the selectors for a component whose url contains the `manifest` string.
pub async fn get_selectors_for_manifest<P: DiagnosticsProvider>(
    manifest: &Option<String>,
    tree_selectors: &Vec<String>,
    accessor: &Option<String>,
    provider: &P,
) -> Result<Vec<String>, Error> {
    let Some(manifest) = manifest.as_ref() else {
        return Ok(tree_selectors.clone());
    };
    let list_command = ListCommand {
        manifest: Some(manifest.clone()),
        with_url: false,
        accessor: accessor.clone(),
    };
    let monikers = list_command
        .execute(provider)
        .await?
        .into_inner()
        .into_iter()
        .map(|item| item.into_moniker())
        .collect::<Vec<_>>();
    if monikers.is_empty() {
        Err(Error::ManifestNotFound(manifest.clone()))
    } else if tree_selectors.is_empty() {
        Ok(monikers.into_iter().map(|moniker| format!("{}:root", moniker)).collect())
    } else {
        Ok(monikers
            .into_iter()
            .flat_map(|moniker| {
                tree_selectors
                    .iter()
                    .map(move |tree_selector| format!("{}:{}", moniker, tree_selector))
            })
            .collect())
    }
}

enum MonikerOrSelector {
    Moniker,
    Selector,
}

/// Checks whether the input string is just a moniker or a full selector
fn moniker_or_selector(untokenized_selector: &str) -> Result<MonikerOrSelector, Error> {
    if untokenized_selector.is_empty() {
        return Err(Error::InvalidSelector("selector strings cannot be empty".to_string()));
    }

    let mut segment_count = 1;
    let mut found_character_in_current_segment = false;
    let mut unparsed_selector_iter = untokenized_selector.char_indices();

    while let Some((_, selector_char)) = unparsed_selector_iter.next() {
        match selector_char {
            escape if escape == selectors::ESCAPE_CHARACTER => {
                if unparsed_selector_iter.next().is_none() {
                    return Err(Error::InvalidSelector(format!(
                        "escape character with no escapee: {}",
                        untokenized_selector
                    )));
                }

                found_character_in_current_segment = true;
            }
            selector_delimiter if selector_delimiter == selectors::SELECTOR_DELIMITER => {
                if !found_character_in_current_segment {
                    return Err(Error::InvalidSelector(format!(
                        "cannot have empty strings delimited by {}",
                        selectors::SELECTOR_DELIMITER
                    )));
                }
                segment_count += 1;
                found_character_in_current_segment = false;
            }
            _ => found_character_in_current_segment = true,
        }
    }

    // ensure that a delimiter wasn't the last thing found
    if !found_character_in_current_segment {
        return Err(Error::InvalidSelector(format!(
            "cannot have empty strings delimited by {}: {}",
            selectors::SELECTOR_DELIMITER,
            untokenized_selector
        )));
    }

    if segment_count == 1 {
        Ok(MonikerOrSelector::Moniker)
    } else {
        Ok(MonikerOrSelector::Selector)
    }
}

/// Expand selectors.
pub fn expand_selectors(selectors: Vec<String>) -> Result<Vec<String>, Error> {
    let mut result = vec![];
    for selector in selectors {
        match moniker_or_selector(&selector)? {
            MonikerOrSelector::Selector => {
                result.push(selector);
            }
            MonikerOrSelector::Moniker => {
                result.push(format!("{}:*", selector));
            }
        }
    }

    Ok(result)
}

/// Helper method to normalize a moniker into its canonical string form. Returns
/// the input moniker unchanged if it cannot be parsed.
pub fn normalize_moniker(moniker: &str) -> String {
    Moniker::parse_str(moniker).map_or(String::from(moniker), |m| m.to_string())
}

/// Get all the exposed `ArchiveAccessor` from any child component which
/// directly exposes them or places them in its outgoing directory.
pub async fn get_accessor_selectors(
    realm_query: &fsys2::RealmQueryProxy,
) -> Result<Vec<String>, Error> {
    let mut result = vec![];
    let instances = get_all_instances(realm_query).await?;
    for instance in instances {
        match get_resolved_declaration(&instance.moniker, realm_query).await {
            Ok(decl) => {
                for capability in decl.capabilities {
                    let capability_name = capability.name().to_string();
                    if !EXPECTED_PROTOCOL_RE.is_match(&capability_name) {
                        continue;
                    }
                    // Skip .host accessors.
                    if capability_name.contains(".host") {
                        continue;
                    }
                    if decl.exposes.iter().any(|expose| expose.source_name() == capability.name()) {
                        let moniker_str = instance.moniker.to_string();
                        let moniker = selectors::sanitize_moniker_for_selectors(&moniker_str);
                        result.push(format!("{moniker}:expose:{capability_name}"));
                    }
                }
            }
            Err(GetDeclarationError::InstanceNotFound(_))
            | Err(GetDeclarationError::InstanceNotResolved(_)) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    result.sort();
    Ok(result)
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use iquery_test_support::MockRealmQuery;
    use std::sync::Arc;

    #[fuchsia::test]
    async fn test_get_accessors() {
        let fake_realm_query = Arc::new(MockRealmQuery::default());
        let realm_query = Arc::clone(&fake_realm_query).get_proxy().await;

        let res = get_accessor_selectors(&realm_query).await;

        assert_matches!(res, Ok(_));

        assert_eq!(
            res.unwrap(),
            vec![
                String::from("example/component:expose:fuchsia.diagnostics.ArchiveAccessor"),
                String::from(
                    "foo/bar/thing\\:instance:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"
                ),
                String::from("foo/component:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
            ]
        );
    }
}
