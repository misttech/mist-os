// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        commands::constants::{IQUERY_TIMEOUT, IQUERY_TIMEOUT_SECS},
        commands::{types::DiagnosticsProvider, Command, ListCommand},
        types::Error,
    },
    anyhow::anyhow,
    cm_rust::*,
    component_debug::{dirs::*, realm::*},
    fidl as _, fidl_fuchsia_sys2 as fsys2, fuchsia_fs,
    futures::StreamExt,
    lazy_static::lazy_static,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, RelativeMoniker, RelativeMonikerBase},
    regex::Regex,
    std::collections::HashSet,
};

lazy_static! {
    static ref EXPECTED_PROTOCOL_RE: &'static str = r".*fuchsia\.diagnostics\..*ArchiveAccessor$";
}

/// Returns the selectors for a component whose url contains the `manifest` string.
pub async fn get_selectors_for_manifest<P: DiagnosticsProvider>(
    manifest: &Option<String>,
    tree_selectors: &Vec<String>,
    accessor: &Option<String>,
    provider: &P,
) -> Result<Vec<String>, Error> {
    match &manifest {
        None => Ok(tree_selectors.clone()),
        Some(manifest) => {
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
    }
}

/// Expand selectors.
pub fn expand_selectors(selectors: Vec<String>) -> Result<Vec<String>, Error> {
    let mut result = vec![];
    for selector in selectors {
        match selectors::tokenize_string(&selector, selectors::SELECTOR_DELIMITER) {
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

/// Helper method to get all `InstanceInfo` from the `RealmExplorer`.
pub(crate) async fn get_instance_infos(
    query_proxy: &fsys2::RealmQueryProxy,
) -> Result<Vec<Instance>, Error> {
    component_debug::realm::get_all_instances(query_proxy)
        .await
        .map_err(|e| Error::CommunicatingWith("RealmExplorer".to_owned(), anyhow!("{:?}", e)))
}

/// Helper method to strip the leading "./" of a relative moniker.
/// Does nothing if the string does not start with "./".
pub async fn strip_leading_relative_moniker(moniker: &str) -> &str {
    return if moniker.starts_with("./") { &moniker[2..] } else { moniker };
}

/// Helper method to append a "./" to moniker. Making it a relative moniker.
/// Does nothing if the string starts with "./".
pub async fn prepend_leading_moniker(moniker: &str) -> String {
    return if moniker.starts_with("./") { moniker.to_owned() } else { format!("./{}", moniker) };
}

/// Get all the exposed `ArchiveAccessor` from any child component which
/// directly exposes them or places them in its outgoing directory.
pub async fn get_accessor_selectors(
    query_proxy: &fsys2::RealmQueryProxy,
    paths: &[String],
) -> Result<Vec<String>, Error> {
    let instance_infos = get_instance_infos(query_proxy).await?;
    let expected_accessor_re = Regex::new(&EXPECTED_PROTOCOL_RE).unwrap();

    let mut output_vec = vec![];

    for ref instance_info in instance_infos {
        let stripped_moniker = instance_info.moniker.to_string();
        let stripped_moniker =
            stripped_moniker.strip_prefix("/").expect("AbsoluteMoniker must start with /");
        if !paths.is_empty() && !paths.iter().any(|path| stripped_moniker.starts_with(path)) {
            // We have a path parameter and the moniker is not matched.
            continue;
        }

        // Use the `RealmQuery `protocol to obtain detailed info for the instances.
        let manifest = match get_manifest(&instance_info.moniker, query_proxy).await {
            Ok(res) => res,
            Err(_) => continue,
        };

        let mut seen_accessors_set = HashSet::new();

        manifest.exposes.iter().for_each(|expose| {
            if let ExposeDecl::Protocol(protocol) = expose {
                let expose_target = protocol.target_name.to_string();
                // Only show directly exposed protocols.
                if expected_accessor_re.is_match(&expose_target)
                    && protocol.source == ExposeSource::Self_
                {
                    // Push "<relative_moniker>:expose:<service_name>" into the output vector.
                    // Stripes out the leading "./".
                    output_vec.push(format!("{}:expose:{}", stripped_moniker, expose_target));
                    seen_accessors_set.insert(expose_target);
                }
            }
        });

        let moniker =
            RelativeMoniker::scope_down(&AbsoluteMoniker::root(), &instance_info.moniker).unwrap();

        let svc_dir = match open_instance_subdir_readable(
            &moniker,
            OpenDirType::Outgoing,
            "svc",
            query_proxy,
        )
        .await
        {
            Ok(dir) => dir,
            Err(_) => continue,
        };

        let entries =
            fuchsia_fs::directory::readdir_recursive(&svc_dir, Some(IQUERY_TIMEOUT.into()))
                .collect::<Vec<_>>()
                .await;

        for ref entry in entries {
            match entry {
                Ok(ref dir_entry) => {
                    let svc_name = dir_entry
                        .name
                        .split_once("/")
                        .map(|(_, name)| name)
                        .unwrap_or(dir_entry.name.as_str());
                    if (dir_entry.kind == fuchsia_fs::directory::DirentKind::File
                        || dir_entry.kind == fuchsia_fs::directory::DirentKind::Service)
                        && !seen_accessors_set.contains(svc_name)
                        && expected_accessor_re.is_match(svc_name)
                    {
                        output_vec.push(format!("{}:out:{}", stripped_moniker, svc_name));
                    }
                }
                Err(fuchsia_fs::directory::RecursiveEnumerateError::Timeout) => {
                    eprintln!(
                        "Warning: Read directory timed out after {} second(s).",
                        IQUERY_TIMEOUT_SECS,
                    );
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Unable to read directory for component {}, error: {:?}.",
                        &stripped_moniker, e
                    );
                }
            }
        }
    }

    output_vec.sort();
    Ok(output_vec)
}

#[cfg(test)]
mod test {
    use {
        super::*, assert_matches::assert_matches, iquery_test_support::MockRealmQuery,
        std::sync::Arc,
    };

    #[fuchsia::test]
    async fn test_get_accessors_no_paths() {
        let fake_realm_query = Arc::new(MockRealmQuery::default());
        let realm_query = Arc::clone(&fake_realm_query).get_proxy().await;

        let res = get_accessor_selectors(&realm_query, &vec![]).await;

        assert_matches!(res, Ok(_));

        assert_eq!(
            res.unwrap(),
            vec![
                String::from("example/component:expose:fuchsia.diagnostics.ArchiveAccessor"),
                String::from("foo/bar/thing:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
                String::from("foo/component:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
                String::from("other/component:out:fuchsia.diagnostics.MagicArchiveAccessor"),
            ]
        );
    }

    #[fuchsia::test]
    async fn test_get_accessors_valid_path() {
        let fake_realm_query = Arc::new(MockRealmQuery::default());
        let realm_query = Arc::clone(&fake_realm_query).get_proxy().await;

        let res = get_accessor_selectors(&realm_query, &vec!["example".to_owned()]).await;

        assert_matches!(res, Ok(_));

        assert_eq!(
            res.unwrap(),
            vec![String::from("example/component:expose:fuchsia.diagnostics.ArchiveAccessor"),]
        );
    }

    #[fuchsia::test]
    async fn test_get_accessors_valid_path_with_slash() {
        let fake_realm_query = Arc::new(MockRealmQuery::default());
        let realm_query = Arc::clone(&fake_realm_query).get_proxy().await;

        let res = get_accessor_selectors(&realm_query, &vec!["example/".to_owned()]).await;

        assert_matches!(res, Ok(_));

        assert_eq!(
            res.unwrap(),
            vec![String::from("example/component:expose:fuchsia.diagnostics.ArchiveAccessor"),]
        );
    }

    #[fuchsia::test]
    async fn test_get_accessors_valid_path_deep() {
        let fake_realm_query = Arc::new(MockRealmQuery::default());
        let realm_query = Arc::clone(&fake_realm_query).get_proxy().await;

        let res = get_accessor_selectors(&realm_query, &vec!["foo/bar/thing".to_owned()]).await;

        assert_matches!(res, Ok(_));

        assert_eq!(
            res.unwrap(),
            vec![String::from("foo/bar/thing:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),]
        );
    }

    #[fuchsia::test]
    async fn test_get_accessors_multi_component() {
        let fake_realm_query = Arc::new(MockRealmQuery::default());
        let realm_query = Arc::clone(&fake_realm_query).get_proxy().await;

        let res = get_accessor_selectors(&realm_query, &vec!["foo/".to_owned()]).await;

        assert_matches!(res, Ok(_));

        assert_eq!(
            res.unwrap(),
            vec![
                String::from("foo/bar/thing:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
                String::from("foo/component:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
            ]
        );
    }
    #[fuchsia::test]
    async fn test_get_accessors_multi_path() {
        let fake_realm_query = Arc::new(MockRealmQuery::default());
        let realm_query = Arc::clone(&fake_realm_query).get_proxy().await;

        let res =
            get_accessor_selectors(&realm_query, &vec!["foo/".to_owned(), "example/".to_owned()])
                .await;

        assert_matches!(res, Ok(_));

        assert_eq!(
            res.unwrap(),
            vec![
                String::from("example/component:expose:fuchsia.diagnostics.ArchiveAccessor"),
                String::from("foo/bar/thing:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
                String::from("foo/component:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
            ]
        );
    }

    #[fuchsia::test]
    async fn test_strip_leading_relative_moniker() {
        assert_eq!(strip_leading_relative_moniker("./example/stuff").await, "example/stuff");
        assert_eq!(strip_leading_relative_moniker("/example/stuff").await, "/example/stuff");
        assert_eq!(strip_leading_relative_moniker("example/stuff").await, "example/stuff");
    }
}
