// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::realm::get_all_instances,
    anyhow::{bail, Result},
    fidl_fuchsia_sys2 as fsys,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase},
};

/// Retrieves a list of CML instance monikers that will match a given string query.
///
/// The string query can be a partial match on the following properties:
/// * component moniker
/// * component URL
/// * component instance ID
// TODO(https://fxbug.dev/114806): `ffx component show` should use this method to get
// component monikers when CMX support has been deprecated.
pub async fn get_cml_monikers_from_query(
    query: &str,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<Vec<AbsoluteMoniker>> {
    // The query parses successfully as an absolute moniker.
    // Assume that the client is interested in a specific component.
    if let Ok(moniker) = AbsoluteMoniker::parse_str(&query) {
        return Ok(vec![moniker]);
    }

    let instances = get_all_instances(realm_query).await?;

    // Try and find instances that contain the query in any of the identifiers
    // (moniker, URL, instance ID).
    let mut monikers: Vec<AbsoluteMoniker> = instances
        .into_iter()
        .filter(|i| {
            let url_match = i.url.contains(&query);
            let moniker_match = i.moniker.to_string().contains(&query);
            let id_match = i.instance_id.as_ref().map_or(false, |id| id.contains(&query));
            url_match || moniker_match || id_match
        })
        .map(|i| i.moniker)
        .collect();

    // For stability guarantees, sort the moniker list
    monikers.sort();

    Ok(monikers)
}

/// Retrieves exactly one CML instance moniker that will match a given string query.
///
/// The string query can be a partial match on the following properties:
/// * component moniker
/// * component URL
/// * component instance ID
///
/// If more than one instance matches the query, an error is thrown.
/// If no instance matches the query, an error is thrown.
pub async fn get_cml_moniker_from_query(
    query: &str,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<AbsoluteMoniker> {
    // Get all instance monikers that match the query and ensure there is only one.
    let mut monikers = get_cml_monikers_from_query(&query, &realm_query).await?;
    if monikers.len() > 1 {
        let monikers: Vec<String> = monikers.into_iter().map(|m| m.to_string()).collect();
        let monikers = monikers.join("\n");
        bail!("The query {:?} matches more than one component instance:\n{}\n\nTo avoid ambiguity, use one of the above monikers instead.", query, monikers);
    }
    if monikers.is_empty() {
        bail!("No matching component instance found for query {:?}.", query);
    }
    let moniker = monikers.remove(0);
    Ok(moniker)
}

#[cfg(test)]
mod tests {
    use {super::*, crate::test_utils::serve_realm_query_instances};

    fn setup_fake_realm_query() -> fsys::RealmQueryProxy {
        serve_realm_query_instances(vec![
            fsys::Instance {
                moniker: Some("./core/foo".to_string()),
                url: Some("#meta/1bar.cm".to_string()),
                instance_id: Some("123456".to_string()),
                resolved_info: None,
                ..Default::default()
            },
            fsys::Instance {
                moniker: Some("./core/boo".to_string()),
                url: Some("#meta/2bar.cm".to_string()),
                instance_id: Some("456789".to_string()),
                resolved_info: None,
                ..Default::default()
            },
        ])
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_monikers_from_query_moniker_more_than_1() {
        let realm_query = setup_fake_realm_query();
        let mut results = get_cml_monikers_from_query("core", &realm_query).await.unwrap();
        assert_eq!(results.len(), 2);

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/core/boo").unwrap());

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/core/foo").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_monikers_from_query_moniker_exactly_1() {
        let realm_query = setup_fake_realm_query();
        let mut results = get_cml_monikers_from_query("foo", &realm_query).await.unwrap();
        assert_eq!(results.len(), 1);

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/core/foo").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_monikers_from_query_url_more_than_1() {
        let realm_query = setup_fake_realm_query();
        let mut results = get_cml_monikers_from_query("bar.cm", &realm_query).await.unwrap();
        assert_eq!(results.len(), 2);

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/core/boo").unwrap());

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/core/foo").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_monikers_from_query_url_exactly_1() {
        let realm_query = setup_fake_realm_query();
        let mut results = get_cml_monikers_from_query("2bar.cm", &realm_query).await.unwrap();
        assert_eq!(results.len(), 1);

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/core/boo").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_monikers_from_query_id_more_than_1() {
        let realm_query = setup_fake_realm_query();
        let mut results = get_cml_monikers_from_query("456", &realm_query).await.unwrap();
        assert_eq!(results.len(), 2);

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/core/boo").unwrap());

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/core/foo").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_monikers_from_query_id_exactly_1() {
        let realm_query = setup_fake_realm_query();
        let mut results = get_cml_monikers_from_query("123", &realm_query).await.unwrap();
        assert_eq!(results.len(), 1);

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/core/foo").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_monikers_from_query_no_results() {
        let realm_query = setup_fake_realm_query();
        let results = get_cml_monikers_from_query("qwerty", &realm_query).await.unwrap();
        assert_eq!(results.len(), 0);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_monikers_from_query_parses_as_moniker() {
        let realm_query = setup_fake_realm_query();
        let mut results = get_cml_monikers_from_query("/qwerty", &realm_query).await.unwrap();
        assert_eq!(results.len(), 1);

        let result = results.remove(0);
        assert_eq!(result, AbsoluteMoniker::parse_str("/qwerty").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_moniker_from_query_parses_as_moniker() {
        let realm_query = setup_fake_realm_query();
        let moniker = get_cml_moniker_from_query("/qwerty", &realm_query).await.unwrap();
        assert_eq!(moniker, AbsoluteMoniker::parse_str("/qwerty").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_moniker_from_query_no_match() {
        let realm_query = setup_fake_realm_query();
        get_cml_moniker_from_query("qwerty", &realm_query).await.unwrap_err();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_moniker_from_query_multiple_match() {
        let realm_query = setup_fake_realm_query();
        get_cml_moniker_from_query("bar.cm", &realm_query).await.unwrap_err();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_moniker_from_query_moniker_single_match() {
        let realm_query = setup_fake_realm_query();
        let moniker = get_cml_moniker_from_query("foo", &realm_query).await.unwrap();
        assert_eq!(moniker, AbsoluteMoniker::parse_str("/core/foo").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_moniker_from_url_moniker_single_match() {
        let realm_query = setup_fake_realm_query();
        let moniker = get_cml_moniker_from_query("2bar.cm", &realm_query).await.unwrap();
        assert_eq!(moniker, AbsoluteMoniker::parse_str("/core/boo").unwrap());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_cml_moniker_from_url_id_single_match() {
        let realm_query = setup_fake_realm_query();
        let moniker = get_cml_moniker_from_query("123", &realm_query).await.unwrap();
        assert_eq!(moniker, AbsoluteMoniker::parse_str("/core/foo").unwrap());
    }
}
