// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::commands::types::DiagnosticsProvider;
use crate::commands::{Command, ListCommand};
use crate::types::Error;
use anyhow::format_err;
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

/// Increments the CharIndices iterator and updates the token builder
/// in order to avoid processing characters being escaped by the selector.
fn handle_escaped_char(
    token_builder: &mut String,
    selection_iter: &mut std::str::CharIndices<'_>,
) -> Result<(), anyhow::Error> {
    token_builder.push(selectors::ESCAPE_CHARACTER);
    let escaped_char_option: Option<(usize, char)> = selection_iter.next();
    match escaped_char_option {
        Some((_, escaped_char)) => token_builder.push(escaped_char),
        None => {
            return Err(format_err!(
                "Selecter fails verification due to unmatched escape character",
            ));
        }
    }
    Ok(())
}

/// Converts a string into a vector of string tokens representing the unparsed
/// string delimited by the provided delimiter, excluded escaped delimiters.
fn tokenize_string(
    untokenized_selector: &str,
    delimiter: char,
) -> Result<Vec<String>, anyhow::Error> {
    let mut token_aggregator = Vec::new();
    let mut curr_token_builder: String = String::new();
    let mut unparsed_selector_iter = untokenized_selector.char_indices();

    while let Some((_, selector_char)) = unparsed_selector_iter.next() {
        match selector_char {
            escape if escape == selectors::ESCAPE_CHARACTER => {
                handle_escaped_char(&mut curr_token_builder, &mut unparsed_selector_iter)?;
            }
            selector_delimiter if selector_delimiter == delimiter => {
                if curr_token_builder.is_empty() {
                    return Err(format_err!(
                        "Cannot have empty strings delimited by {}",
                        delimiter
                    ));
                }
                token_aggregator.push(curr_token_builder);
                curr_token_builder = String::new();
            }
            _ => curr_token_builder.push(selector_char),
        }
    }

    // Push the last section of the selector into the aggregator since we don't delimit the
    // end of the selector.
    if curr_token_builder.is_empty() {
        return Err(format_err!(
            "Cannot have empty strings delimited by {}: {}",
            delimiter,
            untokenized_selector
        ));
    }

    token_aggregator.push(curr_token_builder);
    return Ok(token_aggregator);
}

/// Expand selectors.
pub fn expand_selectors(selectors: Vec<String>) -> Result<Vec<String>, Error> {
    let mut result = vec![];
    for selector in selectors {
        match tokenize_string(&selector, selectors::SELECTOR_DELIMITER) {
            Ok(tokens) => {
                if tokens.len() > 1 {
                    result.push(selector);
                } else if tokens.len() == 1 {
                    result.push(format!("{}:*", selector));
                } else {
                    return Err(Error::InvalidArguments(format!(
                        "Iquery selectors cannot be empty strings: {:?}",
                        selector
                    )));
                }
            }
            Err(e) => {
                return Err(Error::InvalidArguments(format!(
                    "Tokenizing a provided selector failed. Error: {:?} Selector: {:?}",
                    e, selector
                )));
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
