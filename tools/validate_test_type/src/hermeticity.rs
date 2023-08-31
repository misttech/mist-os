// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, format_err, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fidl::unpersist;
use fidl_fuchsia_component_decl::Component;
use fidl_fuchsia_data as fdata;
use fidl_fuchsia_data::Dictionary;
use fuchsia_archive;
use fuchsia_pkg;
use fuchsia_url::AbsoluteComponentUrl;
use rayon::prelude::*;
use std::collections::HashMap;

use crate::{
    CategorizedTestInfo, FailureReason, HermeticityStatus, TestPackageInfo, ValidationStatus,
};

const META_FAR_PREFIX: &'static str = "meta/";
const TEST_REALM_FACET_NAME: &'static str = "fuchsia.test.type";
const TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY: &'static str =
    "fuchsia.test.deprecated-allowed-packages";
const HERMETIC_TEST_REALM: &'static str = "hermetic";

/// Validate that every test in the list of tests is a packaged fuchsia test, and
/// that the test is hermetic.
pub(crate) fn validate_hermeticity(
    categorized_tests: Vec<CategorizedTestInfo>,
    component_test_realms: &HashMap<String, String>,
    build_dir: &Utf8Path,
) -> Vec<ValidationStatus> {
    categorized_tests
        .into_par_iter()
        .map(|categorized_test_info| {
            match categorized_test_info {
                // It's a test package, so validate that the component in the test package is
                // not listed in the lookup table of components that run in test realms:
                CategorizedTestInfo::Package(test) => {
                    if component_test_realms.get(&test.component_label).is_some() {
                        // It has a test realm, so it's not hermetic.
                        ValidationStatus::failed_not_hermetic(test.into())
                    } else {
                        // It doesn't specify a test realm in test_components.json, but it might
                        // still be non-hermetic.
                        //
                        // So we also check the manifest to see if it lists a test realm. This in a
                        // separate function so that it is easy to remove when we are done with
                        // migrations.
                        match check_manifest_hermeticity(&test, build_dir) {
                            Ok(status) => match status {
                                HermeticityStatus::Hermetic => ValidationStatus::Passed,
                                HermeticityStatus::NotHermetic => {
                                    ValidationStatus::failed_not_hermetic(test.into())
                                }
                            },
                            Err(error) => ValidationStatus::failed_with_error(test.into(), error),
                        }
                    }
                }
                CategorizedTestInfo::Host(test) => {
                    ValidationStatus::Failed { test: test.into(), reason: FailureReason::HostTest }
                }
            }
        })
        .collect::<Vec<_>>()
}

/// Given a TestPackageInfo, locate the package manifest, and from it, the
/// meta.far, and from it, the component manifest, and then validate that it
/// doesn't specify a non-hermetic test realm in the component facets.
// TODO(fxbug.dev/118042), TODO(fxbug.dev/117504): Remove these checks when hermeticity no longer
// depends on component manifest
fn check_manifest_hermeticity(
    test: &TestPackageInfo,
    build_dir: &Utf8Path,
) -> Result<HermeticityStatus> {
    if test.package_manifests.len() > 0 {
        let pkg_manifest = &test.package_manifests[0];
        let res = find_meta_far(build_dir, pkg_manifest);
        if res.is_err() {
            return Err(format_err!(
                "error finding meta.far file in package manifest {}: {:?}",
                &pkg_manifest,
                res.unwrap_err()
            ));
        }

        let meta_far_path = res.unwrap();
        let pkg_url = AbsoluteComponentUrl::parse(&test.package_url)?;
        let cm_path = pkg_url.resource();

        let decl = cm_decl_from_meta_far(&meta_far_path, cm_path)?;
        let facets = decl.facets.unwrap_or(fdata::Dictionary::default());
        return check_facet_hermeticity(&facets).map_err(Into::into);
    }
    Err(anyhow!("Missing package_manifests[] entry"))
}

/// Given the path to a package_manifest.json, find the meta.far itself.
fn find_meta_far(build_dir: &Utf8Path, manifest_path: &String) -> Result<Utf8PathBuf> {
    let package_manifest =
        fuchsia_pkg::PackageManifest::try_load_from(build_dir.join(manifest_path))?;

    for blob in package_manifest.blobs() {
        if blob.path.eq(META_FAR_PREFIX) {
            return Ok(build_dir.join(&blob.source_path));
        }
    }
    Err(anyhow!("Missing blob for meta.far"))
}

/// Given the path to a meta.far, and the path of a compiled component manifest
/// within it, return the Component decl parsed.
fn cm_decl_from_meta_far(meta_far_path: &Utf8PathBuf, cm_path: &str) -> Result<Component> {
    let mut meta_far = std::fs::File::open(meta_far_path)?;
    let mut far_reader = fuchsia_archive::Utf8Reader::new(&mut meta_far)?;
    let cm_contents = far_reader.read_file(cm_path)?;
    let decl: Component = unpersist(&cm_contents)?;
    Ok(decl)
}

/// Given a the facets of a component decl, see if it has a test-realm facet,
/// and if that facet is the hermetic test realm or not.
fn check_facet_hermeticity(facets: &Dictionary) -> Result<HermeticityStatus> {
    for facet in facets.entries.as_ref().unwrap_or(&vec![]) {
        if facet.key.eq(TEST_REALM_FACET_NAME) {
            // It's using the test realm facet, so validate that the realm it
            // specifies is the hermetic one.

            let val = facet.value.as_ref().ok_or(anyhow!("Null facet"))?;
            match &**val {
                fdata::DictionaryValue::Str(s) => {
                    if s.ne(HERMETIC_TEST_REALM) {
                        return Ok(HermeticityStatus::NotHermetic);
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "Invalid facet value for {}: {:?}",
                        facet.key.clone(),
                        val
                    ));
                }
            }
        } else if facet.key.eq(TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY) {
            // It's using the deprecated key, so if it specifies a value other
            // than the empty string, it's a non-hermetic test.
            let val = facet.value.as_ref().ok_or(anyhow!("Null facet"))?;
            match &**val {
                fdata::DictionaryValue::StrVec(s) => {
                    if !s.is_empty() {
                        return Ok(HermeticityStatus::NotHermetic);
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "Invalid facet value for {}: {:?}",
                        facet.key.clone(),
                        val
                    ));
                }
            }
        }
    }
    // It didn't make any claims around test realms, so can only run in the
    // hermetic test realm.
    Ok(HermeticityStatus::Hermetic)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod fn_check_facet_hermeticity {
        use super::*;

        #[test]
        fn empty_facet() {
            let facets = Dictionary::default();
            let status = check_facet_hermeticity(&facets).unwrap();
            assert_eq!(status, HermeticityStatus::Hermetic);
        }

        #[test]
        fn hermetic_fuchsia_facet() {
            let mut facets = Dictionary::default();
            facets.entries = Some(vec![fdata::DictionaryEntry {
                key: TEST_REALM_FACET_NAME.to_string(),
                value: Some(Box::new(fdata::DictionaryValue::Str(HERMETIC_TEST_REALM.to_string()))),
            }]);
            let status = check_facet_hermeticity(&facets).unwrap();
            assert_eq!(status, HermeticityStatus::Hermetic);
        }

        #[test]
        fn non_hermetic_fuchsia_facet() {
            let mut facets = Dictionary::default();
            facets.entries = Some(vec![fdata::DictionaryEntry {
                key: TEST_REALM_FACET_NAME.to_string(),
                value: Some(Box::new(fdata::DictionaryValue::Str("some_realm".to_string()))),
            }]);
            let status = check_facet_hermeticity(&facets).unwrap();
            assert_eq!(status, HermeticityStatus::NotHermetic);
        }

        #[test]
        fn invalid_fuchsia_type_facet() {
            let mut facets = Dictionary::default();
            let val = fdata::DictionaryValue::StrVec(vec!["some_realm".to_string()]);
            facets.entries = Some(vec![fdata::DictionaryEntry {
                key: TEST_REALM_FACET_NAME.to_string(),
                value: Some(Box::new(val.clone())),
            }]);
            let result = check_facet_hermeticity(&facets);
            let error = result.unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("Invalid facet value for {}: {:?}", TEST_REALM_FACET_NAME, val)
            );
        }

        #[test]
        fn null_fuchsia_type_facet() {
            let mut facets = Dictionary::default();
            facets.entries = Some(vec![fdata::DictionaryEntry {
                key: TEST_REALM_FACET_NAME.to_string(),
                value: None,
            }]);
            let result = check_facet_hermeticity(&facets);
            let error = result.unwrap_err();
            assert_eq!(error.to_string(), "Null facet");
        }

        #[test]
        fn empty_deprecated_allowed_facet() {
            let mut facets = Dictionary::default();
            facets.entries = Some(vec![fdata::DictionaryEntry {
                key: TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY.to_string(),
                value: Some(Box::new(fdata::DictionaryValue::StrVec(vec![]))),
            }]);
            let status = check_facet_hermeticity(&facets).unwrap();
            assert_eq!(status, HermeticityStatus::Hermetic);
        }

        #[test]
        fn with_deprecated_allowed_facet() {
            let mut facets = Dictionary::default();
            facets.entries = Some(vec![fdata::DictionaryEntry {
                key: TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY.to_string(),
                value: Some(Box::new(fdata::DictionaryValue::StrVec(vec!["some_pkg".into()]))),
            }]);
            let status = check_facet_hermeticity(&facets).unwrap();
            assert_eq!(status, HermeticityStatus::NotHermetic);
        }

        #[test]
        fn invalid_deprecated_allowed_facet() {
            let mut facets = Dictionary::default();
            let val = fdata::DictionaryValue::Str("some_pkg".into());
            facets.entries = Some(vec![fdata::DictionaryEntry {
                key: TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY.to_string(),
                value: Some(Box::new(val.clone())),
            }]);
            let result = check_facet_hermeticity(&facets);
            let error = result.unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "Invalid facet value for {}: {:?}",
                    TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY, val
                )
            );
        }

        #[test]
        fn null_deprecated_allowed_facet() {
            let mut facets = Dictionary::default();
            facets.entries = Some(vec![fdata::DictionaryEntry {
                key: TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY.to_string(),
                value: None,
            }]);
            let result = check_facet_hermeticity(&facets);
            let error = result.unwrap_err();
            assert_eq!(error.to_string(), "Null facet");
        }
    }

    #[test]
    fn arbitrary_facet() {
        let mut facets = Dictionary::default();
        facets.entries = Some(vec![fdata::DictionaryEntry {
            key: "fuchsia.test.some_key".to_string(),
            value: Some(Box::new(fdata::DictionaryValue::StrVec(vec!["some_pkg".into()]))),
        }]);
        let status = check_facet_hermeticity(&facets).unwrap();
        assert_eq!(status, HermeticityStatus::Hermetic);
    }

    mod fn_check_hermeticity {
        use crate::{TestComponentEntry, TestComponentsJsonEntry, TestEntry, TestsJsonEntry};

        use super::*;
        use assert_matches::assert_matches;

        #[test]
        fn not_hermetic() {
            let test_component_entry = TestComponentsJsonEntry {
                test_component: TestComponentEntry {
                    component_label: "component_label".to_string(),
                    moniker: "/some/moniker".to_string(),
                },
            };
            let test_components_map =
                TestComponentsJsonEntry::convert_to_map(vec![test_component_entry]).unwrap();

            let test_entry = TestsJsonEntry {
                test: TestEntry {
                    name: "test name".to_string(),
                    test_label: "//gn/label/for:test".to_string(),
                    package_url: Some(
                        "fuchsia-pkg://fuchsia.com/test_package#meta/test_component.cm".to_string(),
                    ),
                    package_label: Some("//gn/label/for:pkg".to_string()),
                    component_label: Some("component_label".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            };

            let test_package_infos =
                vec![TestPackageInfo::try_from(test_entry.test).unwrap().into()];

            let statuses =
                validate_hermeticity(test_package_infos, &test_components_map, "igored".into());

            assert_matches!(
                statuses.get(0).unwrap(),
                ValidationStatus::Failed { reason: FailureReason::NotHermetic, .. }
            );
        }
    }
}
