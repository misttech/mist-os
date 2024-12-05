// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::commands::types::{Command, DiagnosticsProvider};
use crate::commands::ListCommand;
use crate::types::Error;
use anyhow::anyhow;
use cm_rust::{ExposeDeclCommon, ExposeSource, SourceName};
use component_debug::dirs::*;
use component_debug::realm::*;
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_diagnostics::{All, ArchiveAccessorMarker, Selector, TreeNames};
use fuchsia_fs::directory;
use moniker::Moniker;
use {fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys2};

const ACCESSORS_DICTIONARY: &str = "diagnostics-accessors";

/// Attempt to connect to the `fuchsia.diagnostics.*ArchiveAccessor` with the selector
/// specified.
pub async fn connect_accessor<P: DiscoverableProtocolMarker>(
    moniker: &Moniker,
    accessor_name: &str,
    proxy: &fsys2::RealmQueryProxy,
) -> Result<P::Proxy, Error> {
    let proxy = connect_to_instance_protocol_at_path::<P>(
        &moniker,
        OpenDirType::Exposed,
        &format!("{ACCESSORS_DICTIONARY}/{accessor_name}"),
        &proxy,
    )
    .await
    .map_err(|e| Error::ConnectToProtocol(accessor_name.to_string(), anyhow!("{:?}", e)))?;
    Ok(proxy)
}

/// Returns the selectors for a component whose url contains the `manifest` string.
pub async fn get_selectors_for_manifest<P: DiagnosticsProvider>(
    manifest: String,
    tree_selectors: Vec<String>,
    accessor: Option<String>,
    provider: &P,
) -> Result<Vec<Selector>, Error> {
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
        Ok(monikers
            .into_iter()
            .map(|moniker| {
                let selector_string = format!("{moniker}:root");
                selectors::parse_verbose(&selector_string)
                    .map_err(|e| Error::ParseSelector(selector_string, e.into()))
            })
            .collect::<Result<Vec<_>, _>>()?)
    } else {
        Ok(monikers
            .into_iter()
            .flat_map(|moniker| {
                tree_selectors.iter().map(move |tree_selector| {
                    let selector_string = format!("{moniker}:{tree_selector}");
                    selectors::parse_verbose(&selector_string)
                        .map_err(|e| Error::ParseSelector(selector_string, e.into()))
                })
            })
            .collect::<Result<Vec<_>, _>>()?)
    }
}

async fn fuzzy_search(
    query: &str,
    realm_query: &fsys2::RealmQueryProxy,
) -> Result<Instance, Error> {
    let mut instances = component_debug::query::get_instances_from_query(query, realm_query)
        .await
        .map_err(Error::FuzzyMatchRealmQuery)?;
    if instances.is_empty() {
        return Err(Error::SearchParameterNotFound(query.to_string()));
    } else if instances.len() > 1 {
        return Err(Error::FuzzyMatchTooManyMatches(
            instances.into_iter().map(|i| i.moniker.to_string()).collect(),
        ));
    }

    Ok(instances.pop().unwrap())
}

pub async fn process_fuzzy_inputs<P: DiagnosticsProvider>(
    queries: impl IntoIterator<Item = String>,
    provider: &P,
) -> Result<Vec<Selector>, Error> {
    let mut queries = queries.into_iter().peekable();
    if queries.peek().is_none() {
        return Ok(vec![]);
    }

    let realm_query = provider.realm_query();
    let mut results = vec![];
    for value in queries {
        match fuzzy_search(&value, realm_query).await {
            // try again in case this is a fully escaped moniker or selector
            Err(Error::SearchParameterNotFound(_)) => {
                // In case they included a tree-selector segment, attempt to parse but don't bail
                // on failure
                if let Ok(selector) = selectors::parse_verbose(&value) {
                    results.push(selector);
                } else {
                    // Note the lack of `sanitize_moniker_for_selectors`. `value` is assumed to
                    // either
                    //   A) Be a component that isn't running; therefore the selector being
                    //      right or wrong is irrelevant
                    //   B) Already be sanitized by the caller
                    let selector_string = format!("{}:root", value);
                    results.push(
                        selectors::parse_verbose(&selector_string)
                            .map_err(|e| Error::ParseSelector(selector_string, e.into()))?,
                    )
                }
            }
            Err(e) => return Err(e),
            Ok(instance) => {
                let selector_string = format!(
                    "{}:root",
                    selectors::sanitize_moniker_for_selectors(instance.moniker.to_string()),
                );
                results.push(
                    selectors::parse_verbose(&selector_string)
                        .map_err(|e| Error::ParseSelector(selector_string, e.into()))?,
                )
            }
        }
    }

    Ok(results)
}

/// Returns the selectors for a component whose url, manifest, or moniker contains the
/// `component` string.
pub async fn process_component_query_with_partial_selectors<P: DiagnosticsProvider>(
    component: String,
    tree_selectors: impl Iterator<Item = String>,
    provider: &P,
) -> Result<Vec<Selector>, Error> {
    let mut tree_selectors = tree_selectors.into_iter().peekable();
    let realm_query = provider.realm_query();
    let instance = fuzzy_search(component.as_str(), realm_query).await?;

    let mut results = vec![];
    if tree_selectors.peek().is_none() {
        let selector_string = format!(
            "{}:root",
            selectors::sanitize_moniker_for_selectors(instance.moniker.to_string())
        );
        results
            .push(selectors::parse_verbose(&selector_string).map_err(Error::PartialSelectorHint)?);
    } else {
        for s in tree_selectors {
            let selector_string = format!(
                "{}:{}",
                selectors::sanitize_moniker_for_selectors(instance.moniker.to_string()),
                s
            );
            results.push(
                selectors::parse_verbose(&selector_string).map_err(Error::PartialSelectorHint)?,
            )
        }
    }

    Ok(results)
}

fn add_tree_name(mut selector: Selector, tree_name: String) -> Result<Selector, Error> {
    match selector.tree_names {
        None => selector.tree_names = Some(TreeNames::Some(vec![tree_name])),
        Some(ref mut names) => match names {
            TreeNames::Some(ref mut names) => {
                if !names.iter().any(|n| n == &tree_name) {
                    names.push(tree_name)
                }
            }
            TreeNames::All(_) => {}
            TreeNames::__SourceBreaking { unknown_ordinal } => {
                let unknown_ordinal = *unknown_ordinal;
                return Err(Error::InvalidSelector(format!(
                    "selector had invalid TreeNames variant {unknown_ordinal}: {:?}",
                    selector,
                )));
            }
        },
    }

    Ok(selector)
}

/// Expand selectors.
pub fn expand_selectors(
    selectors: Vec<Selector>,
    tree_name: Option<String>,
) -> Result<Vec<Selector>, Error> {
    let mut result = vec![];

    if selectors.is_empty() {
        let Some(tree_name) = tree_name else {
            return Ok(result);
        };

        // Safety: "**:*" is a valid selector
        let mut selector = selectors::parse_verbose("**:*").unwrap();
        selector.tree_names = Some(TreeNames::Some(vec![tree_name]));
        return Ok(vec![selector]);
    }

    for mut selector in selectors {
        if let Some(tree_name) = &tree_name {
            selector = add_tree_name(selector, tree_name.clone())?;
        } else if selector.tree_names.is_none() {
            selector.tree_names = Some(TreeNames::All(All {}))
        }
        result.push(selector)
    }

    Ok(result)
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
            Err(GetDeclarationError::InstanceNotFound(_))
            | Err(GetDeclarationError::InstanceNotResolved(_)) => continue,
            Err(err) => return Err(err.into()),
            Ok(decl) => {
                for capability in decl.capabilities {
                    let capability_name = capability.name().to_string();
                    if capability_name != ACCESSORS_DICTIONARY {
                        continue;
                    }
                    if !decl.exposes.iter().any(|expose| {
                        expose.source_name() == capability.name()
                            && *expose.source() == ExposeSource::Self_
                    }) {
                        continue;
                    }

                    let Ok(dir_proxy) = open_instance_subdir_readable(
                        &instance.moniker,
                        OpenDirType::Exposed,
                        ACCESSORS_DICTIONARY,
                        realm_query,
                    )
                    .await
                    else {
                        continue;
                    };

                    let Ok(entries) = directory::readdir(&dir_proxy).await else {
                        continue;
                    };

                    for entry in entries {
                        let directory::DirEntry { name, kind: fio::DirentType::Service } = entry
                        else {
                            continue;
                        };
                        // This skips .host accessors intentionally.
                        if !name.starts_with(ArchiveAccessorMarker::PROTOCOL_NAME) {
                            continue;
                        }
                        result.push(format!("{}:{name}", instance.moniker));
                    }
                }
            }
        }
    }
    result.sort();
    Ok(result)
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use iquery_test_support::{MockRealmQuery, MockRealmQueryBuilder};
    use selectors::parse_verbose;
    use std::rc::Rc;

    #[fuchsia::test]
    async fn test_get_accessors() {
        let fake_realm_query = Rc::new(MockRealmQuery::default());
        let realm_query = Rc::clone(&fake_realm_query).get_proxy().await;

        let res = get_accessor_selectors(&realm_query).await;

        assert_matches!(res, Ok(_));

        assert_eq!(
            res.unwrap(),
            vec![
                String::from("example/component:fuchsia.diagnostics.ArchiveAccessor"),
                String::from("foo/bar/thing:instance:fuchsia.diagnostics.ArchiveAccessor.feedback"),
                String::from("foo/component:fuchsia.diagnostics.ArchiveAccessor.feedback"),
            ]
        );
    }

    #[fuchsia::test]
    fn test_expand_selectors() {
        let name = Some("abc".to_string());

        let expected = vec![
            parse_verbose("core/one:[name=abc]root").unwrap(),
            parse_verbose("core/one:[name=xyz, name=abc]root").unwrap(),
        ];

        let actual = expand_selectors(
            vec![
                parse_verbose("core/one:root").unwrap(),
                parse_verbose("core/one:[name=xyz]root").unwrap(),
            ],
            name.clone(),
        )
        .unwrap();

        assert_eq!(actual, expected);

        let expected = vec![
            parse_verbose("core/one:[...]root").unwrap(),
            parse_verbose("core/one:[name=xyz]root").unwrap(),
        ];

        let actual = expand_selectors(
            vec![
                parse_verbose("core/one:root").unwrap(),
                parse_verbose("core/one:[name=xyz]root").unwrap(),
            ],
            None,
        )
        .unwrap();

        assert_eq!(actual, expected);

        let expected = vec![parse_verbose("**:[name=abc]*").unwrap()];
        let actual = expand_selectors(vec![], name).unwrap();
        assert_eq!(actual, expected);

        assert_eq!(expand_selectors(vec![], None).unwrap(), vec![]);
    }

    struct FakeProvider {
        realm_query: fsys2::RealmQueryProxy,
    }

    impl FakeProvider {
        async fn new(monikers: &'static [&'static str]) -> Self {
            let mut builder = MockRealmQueryBuilder::default();
            for name in monikers {
                builder = builder.when(name).moniker(name).add();
            }
            let realm_query_proxy = Rc::new(builder.build()).get_proxy().await;
            Self { realm_query: realm_query_proxy }
        }
    }

    impl DiagnosticsProvider for FakeProvider {
        async fn snapshot<D: diagnostics_data::DiagnosticsData>(
            &self,
            _: Option<&str>,
            _: impl IntoIterator<Item = Selector>,
        ) -> Result<Vec<diagnostics_data::Data<D>>, Error> {
            unreachable!("unimplemented");
        }

        async fn get_accessor_paths(&self) -> Result<Vec<String>, Error> {
            unreachable!("unimplemented");
        }

        fn realm_query(&self) -> &fsys2::RealmQueryProxy {
            &self.realm_query
        }
    }

    #[fuchsia::test]
    async fn test_process_fuzzy_inputs_success() {
        let actual = process_fuzzy_inputs(
            ["moniker1".to_string()],
            &FakeProvider::new(&["core/moniker1", "core/moniker2"]).await,
        )
        .await
        .unwrap();

        let expected = vec![parse_verbose("core/moniker1:root").unwrap()];

        assert_eq!(actual, expected);

        let actual = process_fuzzy_inputs(
            ["moniker1:collection".to_string()],
            &FakeProvider::new(&["core/moniker1:collection", "core/moniker1", "core/moniker2"])
                .await,
        )
        .await
        .unwrap();

        let expected = vec![parse_verbose(r"core/moniker1\:collection:root").unwrap()];

        assert_eq!(actual, expected);

        let actual = process_fuzzy_inputs(
            [r"core/moniker1\:collection".to_string()],
            &FakeProvider::new(&["core/moniker1:collection"]).await,
        )
        .await
        .unwrap();

        let expected = vec![parse_verbose(r"core/moniker1\:collection:root").unwrap()];

        assert_eq!(actual, expected);

        let actual = process_fuzzy_inputs(
            ["core/moniker1:root:prop".to_string()],
            &FakeProvider::new(&["core/moniker1:collection", "core/moniker1"]).await,
        )
        .await
        .unwrap();

        let expected = vec![parse_verbose(r"core/moniker1:root:prop").unwrap()];

        assert_eq!(actual, expected);

        let actual = process_fuzzy_inputs(
            ["core/moniker1".to_string(), "core/moniker2".to_string()],
            &FakeProvider::new(&["core/moniker1", "core/moniker2"]).await,
        )
        .await
        .unwrap();

        let expected = vec![
            parse_verbose(r"core/moniker1:root").unwrap(),
            parse_verbose(r"core/moniker2:root").unwrap(),
        ];

        assert_eq!(actual, expected);

        let actual = process_fuzzy_inputs(
            ["moniker1".to_string(), "moniker2".to_string()],
            &FakeProvider::new(&["core/moniker1"]).await,
        )
        .await
        .unwrap();

        let expected = vec![
            parse_verbose(r"core/moniker1:root").unwrap(),
            // fallback is to assume that moniker2 is a valid moniker
            parse_verbose("moniker2:root").unwrap(),
        ];

        assert_eq!(actual, expected);

        let actual = process_fuzzy_inputs(
            ["core/moniker1:root:prop".to_string(), "core/moniker2".to_string()],
            &FakeProvider::new(&["core/moniker1", "core/moniker2"]).await,
        )
        .await
        .unwrap();

        let expected = vec![
            parse_verbose(r"core/moniker1:root:prop").unwrap(),
            parse_verbose(r"core/moniker2:root").unwrap(),
        ];

        assert_eq!(actual, expected);
    }

    #[fuchsia::test]
    async fn test_process_fuzzy_inputs_failures() {
        let actual =
            process_fuzzy_inputs(["moniker ".to_string()], &FakeProvider::new(&["moniker"]).await)
                .await;

        assert_matches!(actual, Err(Error::ParseSelector(_, _)));

        let actual = process_fuzzy_inputs(
            ["moniker".to_string()],
            &FakeProvider::new(&["core/moniker1", "core/moniker2"]).await,
        )
        .await;

        assert_matches!(actual, Err(Error::FuzzyMatchTooManyMatches(_)));
    }

    #[fuchsia::test]
    async fn test_fuzzy_component_search() {
        let actual = process_component_query_with_partial_selectors(
            "moniker1".to_string(),
            [].into_iter(),
            &FakeProvider::new(&["core/moniker1", "core/moniker2"]).await,
        )
        .await
        .unwrap();

        let expected = vec![parse_verbose(r"core/moniker1:root").unwrap()];

        assert_eq!(actual, expected);

        let actual = process_component_query_with_partial_selectors(
            "moniker1".to_string(),
            ["root/foo:bar".to_string()].into_iter(),
            &FakeProvider::new(&["core/moniker1", "core/moniker2"]).await,
        )
        .await
        .unwrap();

        let expected = vec![parse_verbose(r"core/moniker1:root/foo:bar").unwrap()];

        assert_eq!(actual, expected);

        let actual = process_component_query_with_partial_selectors(
            "moniker1".to_string(),
            ["root/foo:bar".to_string()].into_iter(),
            &FakeProvider::new(&["core/moniker2", "core/moniker3"]).await,
        )
        .await;

        assert_matches!(actual, Err(Error::SearchParameterNotFound(_)));
    }
}
