// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::BTreeSet;

use anyhow::{anyhow, format_err, Context, Error, Result};
use camino::Utf8PathBuf;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cmp::{Eq, PartialEq};
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs;
use std::io::Read;
use structopt::StructOpt;

mod hermeticity;
mod opts;

//========
//
// The following structs are used for deserializing the tests.json and test_components.json
// files.
//

// TODO(https://fxbug.dev/42082651): Refactor test_list_tool and reuse these structures and their parser
// functions.

/// Deserialization wrapper for 'tests.json'
#[derive(Debug, Default, Serialize, Deserialize)]
struct TestsJsonEntry {
    test: TestEntry,
    product_bundle: Option<String>,
    environments: Vec<Environments>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Environments {
    dimensions: Dimensions,

    /// The test tags.
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Dimensions {
    /// Tests that require a device will have a device_type specified.
    device_type: Option<String>,

    /// Tests that run on a particular OS (other than Fuchsia) will specify that
    /// OS. (ie, host tests)
    os: Option<String>,

    /// The CPU type that the host test is compiled for (x64, arm64, etc.)
    cpu: Option<String>,
}

/// Test entries from 'tests.json'
#[derive(Debug, Serialize, Deserialize, Default)]
struct TestEntry {
    // All tests have these fields
    name: String,
    #[serde(rename = "label")]
    test_label: String,

    // Only fuchsia_test_package() tests have these fields.
    // TODO(https://fxbug.dev/42082651): Split this struct into an enum type.
    package_url: Option<String>,
    component_label: Option<String>,
    package_label: Option<String>,
    #[serde(default)]
    package_manifests: Vec<String>,

    // Only host_test() tests have these fields.
    os: Option<String>,
    path: Option<Utf8PathBuf>,
    runtime_deps: Option<Utf8PathBuf>,
}

/// Deserialization wrapper for 'test_components.json'
#[derive(Debug, Serialize, Deserialize)]
struct TestComponentsJsonEntry {
    test_component: TestComponentEntry,
}

/// The moniker of a test realm to run a non-hermetic test in.  This is just to
/// clarify usages.
type RealmMoniker = String;

/// Only non-hermetic tests will have an entry in 'test_components.json', and
/// it will contain the moniker of the realm to run the test in.
#[derive(Debug, Serialize, Deserialize, Default)]
struct TestComponentEntry {
    /// This is the label of test component.
    #[serde(rename = "label")]
    component_label: String,

    /// The moniker of the test realm to run the test component in.
    moniker: RealmMoniker,
}

impl TestComponentsJsonEntry {
    /// Create a lookup map of GN labels for a test component to the moniker of
    /// the realm to run it in.
    fn convert_to_map(
        value: Vec<TestComponentsJsonEntry>,
    ) -> Result<HashMap<String, RealmMoniker>, Error> {
        let mut map = HashMap::<String, String>::default();
        for entry in value {
            if let Some(old_entry) = map.get(&entry.test_component.component_label) {
                if !old_entry.eq(&entry.test_component.moniker) {
                    return Err(format_err!(
                        "Conflicting test components: {:?}, {:?}",
                        old_entry,
                        entry
                    ));
                }
            } else {
                map.insert(entry.test_component.component_label, entry.test_component.moniker);
            }
        }
        Ok(map)
    }
}

fn read_tests_json(file: &Utf8PathBuf) -> Result<Vec<TestsJsonEntry>> {
    let mut buffer = String::new();
    fs::File::open(&file)?.read_to_string(&mut buffer)?;
    let t: Vec<TestsJsonEntry> = serde_json::from_str(&buffer)?;
    Ok(t)
}

fn read_test_components_json(file: &Utf8PathBuf) -> Result<Vec<TestComponentsJsonEntry>> {
    let mut buffer = String::new();
    fs::File::open(&file)?.read_to_string(&mut buffer)?;
    let t: Vec<TestComponentsJsonEntry> = serde_json::from_str(&buffer)?;
    Ok(t)
}

//========
//
// The parsed json from the above structs is converted into the following
// structs to make the data more usable in the validation process.
//

/// Different categories of tests have different information.
#[derive(Debug, PartialEq)]
enum CategorizedTestInfo {
    Package(TestPackageInfo),
    Host(HostTestInfo),
    E2e(E2eTestInfo),
    DisabledHost(HostTestInfo),
}

impl CategorizedTestInfo {
    fn basic_info(&self) -> &BasicTestInfo {
        match self {
            Self::Package(t) => &t.basic_info,
            Self::Host(t) => &t.basic_info,
            Self::E2e(t) => &t.basic_info,
            Self::DisabledHost(t) => &t.basic_info,
        }
    }

    fn name(&self) -> &String {
        &self.basic_info().name
    }

    fn label(&self) -> &String {
        &self.basic_info().test_label
    }
}

impl From<TestPackageInfo> for CategorizedTestInfo {
    fn from(value: TestPackageInfo) -> Self {
        Self::Package(value)
    }
}
impl From<HostTestInfo> for CategorizedTestInfo {
    fn from(value: HostTestInfo) -> Self {
        Self::Host(value)
    }
}
impl From<E2eTestInfo> for CategorizedTestInfo {
    fn from(value: E2eTestInfo) -> Self {
        Self::E2e(value)
    }
}

/// Information common to all tests
#[derive(Debug, PartialEq)]
struct BasicTestInfo {
    /// The name of this test
    name: String,

    /// The GN label of the target for this test.
    test_label: String,
}

/// Information describing a test in a Fuchsia Package.
#[derive(Debug, PartialEq)]
struct TestPackageInfo {
    /// The common, basic, test info
    basic_info: BasicTestInfo,

    /// The 'fuchsia-pkg://....' url for this test package.
    package_url: String,

    /// The GN label for the 'fuchsia_test_component()' target for this test.
    /// Note: This can be None in cases like prebuilt test packages.
    component_label: Option<String>,

    /// Other package manifests.
    package_manifests: Vec<String>,
}

impl TryFrom<TestEntry> for TestPackageInfo {
    type Error = Error;

    fn try_from(value: TestEntry) -> Result<Self, Self::Error> {
        let TestEntry { name, test_label, package_url, component_label, package_manifests, .. } =
            value;

        let package_url = package_url.ok_or_else(|| anyhow!("Missing 'package_url' field"))?;

        Ok(Self {
            basic_info: BasicTestInfo { name, test_label },
            package_url,
            component_label,
            package_manifests,
        })
    }
}

/// Information describing a host test.
#[derive(Debug, PartialEq)]
struct HostTestInfo {
    /// The common, basic, test info
    basic_info: BasicTestInfo,

    /// The path to the executable for this host test.
    path: Utf8PathBuf,

    /// The path to the list of host_test_data() entries for this test.
    runtime_deps: Option<Utf8PathBuf>,
}

impl TryFrom<TestEntry> for HostTestInfo {
    type Error = Error;

    fn try_from(value: TestEntry) -> Result<Self, Self::Error> {
        let TestEntry { name, test_label, path, runtime_deps, .. } = value;

        Ok(Self {
            basic_info: BasicTestInfo { name, test_label },
            path: path.ok_or_else(|| anyhow!("No path"))?,
            runtime_deps,
        })
    }
}

/// Information describing an e2e test.
#[derive(Debug, PartialEq)]
struct E2eTestInfo {
    /// The common, basic, test info
    basic_info: BasicTestInfo,

    /// The path to the executable for this host test.
    path: Utf8PathBuf,

    /// The path to the list of host_test_data() entries for this test.
    runtime_deps: Option<Utf8PathBuf>,

    /// The device type that this test is meant to work with.
    device_type: String,

    /// The tags for this test
    tags: Vec<String>,
}

impl TryFrom<(TestEntry, String, Vec<String>)> for E2eTestInfo {
    type Error = Error;

    fn try_from(value: (TestEntry, String, Vec<String>)) -> Result<Self, Self::Error> {
        let (TestEntry { name, test_label, path, runtime_deps, .. }, device_type, tags) = value;

        Ok(Self {
            basic_info: BasicTestInfo { name, test_label },
            path: path.ok_or_else(|| anyhow!("No path"))?,
            runtime_deps,
            device_type,
            tags,
        })
    }
}

/// Sort out what kind of test each is:
///   - test package
///   - host test
///   - end-2-end test (TODO: need more info for this)
///
fn categorize_tests(tests_json: Vec<TestsJsonEntry>) -> Result<Vec<CategorizedTestInfo>> {
    tests_json
        .into_par_iter()
        .map(|mut entry| {
            let label = entry.test.test_label.clone();
            if entry.test.package_url.is_some() {
                Ok(TestPackageInfo::try_from(entry.test)
                    .with_context(|| format!("test: {}", label))?
                    .into())
            } else {
                match entry.test.os.as_deref() {
                    Some("linux") | Some("mac") => {
                        // It's a test that runs on the host. It could either be a host-only or an
                        // e2e test, however.  If the dimensions indicate a device is required, then
                        // it's definitely an e2e test.
                        match entry.environments.pop() {
                            None => {
                                // This is a disabled test, and without seeing the environment, we
                                // cannot determine if it's host-only or an end-to-end test, so just
                                // classify it as a disabled host test, and carry on.
                                Ok(CategorizedTestInfo::DisabledHost(
                                    HostTestInfo::try_from(entry.test)
                                        .with_context(|| format!("test: {}", label))?,
                                ))
                            }
                            Some(Environments { dimensions, tags, .. }) => {
                                match dimensions.device_type {
                                    Some(device_type) =>
                                    // It's an e2e test.
                                    {
                                        Ok(E2eTestInfo::try_from((entry.test, device_type, tags))
                                            .with_context(|| format!("test: {}", label))?
                                            .into())
                                    }

                                    None =>
                                    // It's a host-only test
                                    {
                                        Ok(HostTestInfo::try_from(entry.test)
                                            .with_context(|| format!("test: {}", label))?
                                            .into())
                                    }
                                }
                            }
                        }
                    }
                    Some("fuchsia") => {
                        match entry.product_bundle {
                            Some(_pb_name) => {
                                match entry.environments.pop() {
                                    None => Ok(E2eTestInfo::try_from((
                                        entry.test,
                                        "NA".to_string(),
                                        vec![],
                                    ))
                                    .with_context(|| format!("test: {}", label))?
                                    .into()),
                                    Some(Environments { dimensions, tags, .. }) => {
                                        match dimensions.device_type {
                                            Some(device_type) =>
                                            // It's an e2e test.
                                            {
                                                Ok(E2eTestInfo::try_from((
                                                    entry.test,
                                                    device_type,
                                                    tags,
                                                ))
                                                .with_context(|| format!("test: {}", label))?
                                                .into())
                                            }
                                            None => Err(anyhow!(
                                                "Unable to categorize test: {:?}",
                                                entry.test.test_label
                                            )),
                                        }
                                    }
                                }
                            }
                            None => Err(anyhow!(
                                "Unable to categorize test: {:?}",
                                entry.test.test_label
                            )),
                        }
                    }
                    _ => Err(anyhow!("Unable to categorize test: {:?}", entry.test.test_label)),
                }
            }
        })
        .collect::<Result<Vec<_>>>()
}

//========
//
// The following structs are used to hold the output of validation.
//

/// For packaged tests, these are the hermeticity statuses that can be determined.
#[derive(Eq, PartialEq, Debug)]
enum HermeticityStatus {
    /// It's a hermetic test at runtime
    Hermetic,

    /// It's not a hermetic test at runtime
    NotHermetic,
}

/// When validating tests, they either pass, or fail with some reason.  This is
/// separate from Result<(), (test, reason)> so that it doesn't get used with
/// the '?' operator, and is distinct from other failures that can be encountered.
#[derive(Debug)]
enum ValidationStatus {
    Passed,
    Failed { test: CategorizedTestInfo, reason: FailureReason },
}

/// Why the test failed verification.
#[derive(Debug)]
enum FailureReason {
    // It's not hermetic
    NotHermetic,

    // It's a Fuchsia Package test
    FuchsiaPackage,

    // It's a host test
    HostTest,

    // It's an e2e test
    E2eTest,

    // An error was encountered during validation (ie, something couldn't be
    // read, or was fatally inconsistent in some other way)
    InternalError(Error),
}

impl ValidationStatus {
    /// Create a ValidationStatus for the given test, due to being non-hermetic.
    fn failed_not_hermetic(test: CategorizedTestInfo) -> Self {
        Self::Failed { test, reason: FailureReason::NotHermetic }
    }

    /// Create a ValidationStatus for the given test, due to encountering some
    /// error during validation.
    fn failed_with_error(test: CategorizedTestInfo, error: Error) -> Self {
        Self::Failed { test, reason: FailureReason::InternalError(error) }
    }
}

fn validate_host_only(categorized_tests: Vec<CategorizedTestInfo>) -> Vec<ValidationStatus> {
    categorized_tests
        .into_iter()
        .map(|test| match test {
            CategorizedTestInfo::Host(_) | CategorizedTestInfo::DisabledHost(_) => {
                ValidationStatus::Passed
            }
            CategorizedTestInfo::Package(t) => {
                ValidationStatus::Failed { test: t.into(), reason: FailureReason::FuchsiaPackage }
            }
            CategorizedTestInfo::E2e(t) => {
                ValidationStatus::Failed { test: t.into(), reason: FailureReason::E2eTest }
            }
        })
        .collect()
}

fn validate_e2e_only(categorized_tests: Vec<CategorizedTestInfo>) -> Vec<ValidationStatus> {
    categorized_tests
        .into_iter()
        .map(|test| match test {
            CategorizedTestInfo::E2e(_) => ValidationStatus::Passed,
            CategorizedTestInfo::Host(t) | CategorizedTestInfo::DisabledHost(t) => {
                ValidationStatus::Failed { test: t.into(), reason: FailureReason::HostTest }
            }
            CategorizedTestInfo::Package(t) => {
                ValidationStatus::Failed { test: t.into(), reason: FailureReason::FuchsiaPackage }
            }
        })
        .collect()
}

fn write_depfile(
    depfile: &Utf8PathBuf,
    output: &Utf8PathBuf,
    inputs: &BTreeSet<Utf8PathBuf>,
) -> Result<(), Error> {
    if inputs.len() == 0 {
        return Ok(());
    }
    let mut contents = vec![format!("{}:", output)];

    for input in inputs {
        contents.push(format!("\\\n  {}", input))
    }
    contents.push("\n".to_string());

    fs::write(depfile, contents.join(""))?;
    Ok(())
}

fn run_tool() -> Result<()> {
    let opt = opts::Opt::from_args();
    opt.validate()?;

    let mut inputs_for_depfile = BTreeSet::<Utf8PathBuf>::new();

    // Deserialize tests.json
    inputs_for_depfile.insert(opt.test_list.clone());
    let tests_json = read_tests_json(&opt.test_list)
        .with_context(|| format!("Parsing test list: {}", &opt.test_list))?;

    // Categorize the tests by type (host, test package, etc.)
    let categorized_tests =
        categorize_tests(tests_json).with_context(|| format!("Categorizing tests"))?;

    // Deserialize test_components.json
    inputs_for_depfile.insert(opt.test_components_list.clone());
    let test_components_json = read_test_components_json(&opt.test_components_list)
        .with_context(|| format!("Parsing test components list: {}", &opt.test_components_list))?;

    // Create a lookup map of GN labels for components to realms.  This only
    // contains entries for non-hermetic tests.
    let component_test_realms = TestComponentsJsonEntry::convert_to_map(test_components_json)
        .with_context(|| format!("Creating Test Components map"))?;

    // For each test, validate it based on the validation option that was specified.
    let validation_results = match opt.validation_type {
        opts::ValidateType::Hermetic => hermeticity::validate_hermeticity(
            categorized_tests,
            &component_test_realms,
            &opt.build_dir,
            &mut inputs_for_depfile,
        ),
        opts::ValidateType::Host => validate_host_only(categorized_tests),
        opts::ValidateType::E2e => validate_e2e_only(categorized_tests),
    };

    // Sort results by validation status

    let mut packaged = vec![];
    let mut host = vec![];
    let mut e2e = vec![];
    let mut not_hermetic = vec![];
    let mut internal_errors = vec![];

    for result in validation_results {
        if let ValidationStatus::Failed { test, reason } = result {
            match reason {
                FailureReason::FuchsiaPackage => packaged.push(test),
                FailureReason::HostTest => host.push(test),
                FailureReason::NotHermetic => not_hermetic.push(test),
                FailureReason::E2eTest => e2e.push(test),
                FailureReason::InternalError(error) => internal_errors.push((test, error)),
            }
        }
    }

    if !not_hermetic.is_empty()
        || !packaged.is_empty()
        || !host.is_empty()
        || !e2e.is_empty()
        || !internal_errors.is_empty()
    {
        // There are validation failures, so print them out:
        println!("\nTests failed validation by type!\n");
        println!("  Validating that test group: {}", opt.test_group_name);
        println!(
            "    - {}",
            match &opt.validation_type {
                opts::ValidateType::Hermetic => "only contains hermetic tests",
                opts::ValidateType::Host => "only contains host-only tests",
                opts::ValidateType::E2e => "only contains e2e tests",
            }
        );

        if !packaged.is_empty() {
            println!("\nThe following tests are packaged Fuchsia tests:");
            for test in packaged {
                println!("  {}", test.label())
            }
        }

        if !host.is_empty() {
            println!("\nThe following tests are host tests:");
            for test in host {
                println!("  {}", test.label())
            }
        }

        if !e2e.is_empty() {
            println!("\nThe following tests are end-to-end tests:");
            for test in e2e {
                println!("  {}", test.label())
            }
        }

        if !not_hermetic.is_empty() {
            println!("\nThe following tests are not hermetic:");
            for test in not_hermetic {
                println!("  {}  -  {}", test.name(), test.label())
            }
        }

        if !internal_errors.is_empty() {
            println!("\nThe following packages could not be validated due to errors:");
            for (test, error) in internal_errors {
                println!("  {}  -  {}  {}", test.name(), test.label(), error)
            }
        }

        println!("");

        std::process::exit(1);
    }

    if let Some(output_path) = opt.output {
        if let Some(parent_dir) = output_path.parent() {
            std::fs::create_dir_all(parent_dir)
                .with_context(|| format!("Creating output dir: {}", parent_dir))?;
        }
        std::fs::write(&output_path, "Ok")
            .with_context(|| format!("Writing output file: {}", output_path))?;

        if let Some(depfile_path) = opt.depfile {
            write_depfile(&depfile_path, &output_path, &inputs_for_depfile)
                .with_context(|| format!("Writing depfile to: {}", depfile_path))?;
        }
    }

    Ok(())
}

fn main() -> Result<(), Error> {
    run_tool().map_err(|e| {
        // Format the anyhow error into a series of lines for context:
        anyhow!(e
            .chain()
            .enumerate()
            .map(|(i, e)| format!("\n  {: >3}.  {}", i + 1, e))
            .collect::<Vec<String>>()
            .concat())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_and_categorized_tests_json() {
        let json = json!([{
          "environments": [
            {
              "dimensions": {
                "cpu": "x64",
                "os": "Linux"
              }
            }
          ],
          "test": {
            "cpu": "x64",
            "label": "//tools/validate_test_type:validate_test_type_test(//build/toolchain:host_x64)",
            "name": "host_x64/validate_test_type_bin_test",
            "os": "linux",
            "path": "host_x64/validate_test_type_bin_test",
            "runtime_deps": "host_x64/gen/tools/validate_test_type/validate_test_type_test.deps.json"
          }
        },
        {
          "environments": [
            {
              "dimensions": {
                "device_type": "AEMU"
              }
            }
          ],
          "test": {
            "build_rule": "fuchsia_test_package",
            "component_label": "//examples/components/config/integration_test:cpp_test(//build/toolchain/fuchsia:x64)",
            "cpu": "x64",
            "label": "//examples/components/config/integration_test:cpp_config_integration_test_test_cpp_test(//build/toolchain/fuchsia:x64)",
            "log_settings": {
              "max_severity": "WARN"
            },
            "name": "fuchsia-pkg://fuchsia.com/cpp_config_integration_test#meta/config_integration_test_cpp.cm",
            "os": "fuchsia",
            "package_label": "//examples/components/config/integration_test:cpp_config_integration_test(//build/toolchain/fuchsia:x64)",
            "package_manifests": [
              "obj/examples/components/config/integration_test/cpp_config_integration_test/package_manifest.json"
            ],
            "package_url": "fuchsia-pkg://fuchsia.com/cpp_config_integration_test#meta/config_integration_test_cpp.cm"
          }
        },
        {
            "environments": [
              {
                "dimensions": {
                  "device_type": "QEMU",
                },
                "tags": [ "tag1", "tag2"]
              }
            ],
            "test": {
                "cpu": "x64",
                "label": "//src/tests/end_to_end/fidlcat:fidlcat_e2e_tests(//build/toolchain:host_x64)",
                "name": "host_x64/obj/src/tests/end_to_end/fidlcat/fidlcat_e2e_tests.sh",
                "os": "linux",
                "path": "host_x64/obj/src/tests/end_to_end/fidlcat/fidlcat_e2e_tests.sh",
                "runtime_deps": "host_x64/gen/src/tests/end_to_end/fidlcat/fidlcat_e2e_tests.deps.json"
            }
          },
          {
            "environments": [
              {
                "dimensions": {
                  "device_type": "Vim3"
                }
              }
            ],
            "product_bundle": "some_product.some_board_vim3",
            "test": {
              "build_rule": "bootfs_test",
              "cpu": "arm64",
              "isolated": true,
              "label": "//src/some/path/to/test:test(//build/toolchain/fuchsia:arm64)",
              "log_settings": {
                "max_severity": "WARN"
              },
              "name": "/boot/test/the-test",
              "os": "fuchsia",
              "path": "/boot/test/the-test",
              "runtime_deps": "gen/src/some/path/to/test/bootfs_test.deps.json"
            }
          },
          {
            "environments": [],
            "product_bundle": "some_product.some_board_vim3",
            "test": {
              "build_rule": "bootfs_test",
              "cpu": "arm64",
              "isolated": true,
              "label": "//src/some/path/to/test:test(//build/toolchain/fuchsia:arm64)",
              "log_settings": {
                "max_severity": "WARN"
              },
              "name": "/boot/test/the-test",
              "os": "fuchsia",
              "path": "/boot/test/the-test",
              "runtime_deps": "gen/src/some/path/to/test/bootfs_test.deps.json"
            }
          }
          ]);
        let parsed: Vec<TestsJsonEntry> = serde_json::from_value(json).unwrap();

        let categorized = categorize_tests(parsed).unwrap();

        let host_test = &categorized[0];
        assert_eq!(host_test.name(), "host_x64/validate_test_type_bin_test");
        assert_eq!(
            host_test.label(),
            "//tools/validate_test_type:validate_test_type_test(//build/toolchain:host_x64)"
        );
        assert_eq!(
            host_test,
            &CategorizedTestInfo::Host(HostTestInfo {
                basic_info: BasicTestInfo {
                    name: "host_x64/validate_test_type_bin_test".into(),
                    test_label:
                        "//tools/validate_test_type:validate_test_type_test(//build/toolchain:host_x64)"
                            .into(),
                },
                path: "host_x64/validate_test_type_bin_test".into(),
                runtime_deps: Some(
                    "host_x64/gen/tools/validate_test_type/validate_test_type_test.deps.json"
                        .into(),
                ),
            })
        );

        let fuchsia_package_test = &categorized[1];
        assert_eq!(
            fuchsia_package_test.name(),
            "fuchsia-pkg://fuchsia.com/cpp_config_integration_test#meta/config_integration_test_cpp.cm"
        );
        assert_eq!(
            fuchsia_package_test.label(),
            "//examples/components/config/integration_test:cpp_config_integration_test_test_cpp_test(//build/toolchain/fuchsia:x64)"
        );
        assert_eq!(
            fuchsia_package_test,
            &CategorizedTestInfo::Package(TestPackageInfo {
                basic_info: BasicTestInfo {
                    name: "fuchsia-pkg://fuchsia.com/cpp_config_integration_test#meta/config_integration_test_cpp.cm".into(),
                    test_label:
                        "//examples/components/config/integration_test:cpp_config_integration_test_test_cpp_test(//build/toolchain/fuchsia:x64)"
                            .into(),
                },
                package_url: "fuchsia-pkg://fuchsia.com/cpp_config_integration_test#meta/config_integration_test_cpp.cm".into(),
                component_label: Some("//examples/components/config/integration_test:cpp_test(//build/toolchain/fuchsia:x64)".into()),
                package_manifests: vec!["obj/examples/components/config/integration_test/cpp_config_integration_test/package_manifest.json".into()]
            })
        );

        let e2e_test = &categorized[2];
        assert_eq!(
            e2e_test.name(),
            "host_x64/obj/src/tests/end_to_end/fidlcat/fidlcat_e2e_tests.sh"
        );
        assert_eq!(
            e2e_test.label(),
            "//src/tests/end_to_end/fidlcat:fidlcat_e2e_tests(//build/toolchain:host_x64)"
        );
        assert_eq!(
            e2e_test,
            &CategorizedTestInfo::E2e(E2eTestInfo {
                basic_info: BasicTestInfo {
                    name: "host_x64/obj/src/tests/end_to_end/fidlcat/fidlcat_e2e_tests.sh".into(),
                    test_label:
                        "//src/tests/end_to_end/fidlcat:fidlcat_e2e_tests(//build/toolchain:host_x64)"
                            .into(),
                },
                path: "host_x64/obj/src/tests/end_to_end/fidlcat/fidlcat_e2e_tests.sh".into(),
                runtime_deps: Some(
                    "host_x64/gen/src/tests/end_to_end/fidlcat/fidlcat_e2e_tests.deps.json".into()
                ),
                device_type: "QEMU".into(),
                tags: vec!["tag1".into(), "tag2".into()]
            })
        );

        let e2e_test_bootfs: &CategorizedTestInfo = &categorized[3];
        assert_eq!(e2e_test_bootfs.name(), "/boot/test/the-test");
        assert_eq!(
            e2e_test_bootfs.label(),
            "//src/some/path/to/test:test(//build/toolchain/fuchsia:arm64)"
        );
        assert_eq!(
            e2e_test_bootfs,
            &CategorizedTestInfo::E2e(E2eTestInfo {
                basic_info: BasicTestInfo {
                    name: "/boot/test/the-test".into(),
                    test_label: "//src/some/path/to/test:test(//build/toolchain/fuchsia:arm64)"
                        .into(),
                },
                path: "/boot/test/the-test".into(),
                runtime_deps: Some("gen/src/some/path/to/test/bootfs_test.deps.json".into()),
                device_type: "Vim3".into(),
                tags: vec![]
            })
        );

        let e2e_test_bootfs_no_env: &CategorizedTestInfo = &categorized[4];
        assert_eq!(e2e_test_bootfs_no_env.name(), "/boot/test/the-test");
        assert_eq!(
            e2e_test_bootfs_no_env.label(),
            "//src/some/path/to/test:test(//build/toolchain/fuchsia:arm64)"
        );
        assert_eq!(
            e2e_test_bootfs_no_env,
            &CategorizedTestInfo::E2e(E2eTestInfo {
                basic_info: BasicTestInfo {
                    name: "/boot/test/the-test".into(),
                    test_label: "//src/some/path/to/test:test(//build/toolchain/fuchsia:arm64)"
                        .into(),
                },
                path: "/boot/test/the-test".into(),
                runtime_deps: Some("gen/src/some/path/to/test/bootfs_test.deps.json".into()),
                device_type: "NA".into(),
                tags: vec![]
            })
        );
    }

    #[test]
    fn test_parse_test_components_json_entry() {
        let json = json!([
        {
          "test_component": {
            "label": "//some/label/for/test:a",
            "moniker": "/core/testing/system-tests"
          }
        },
        {
          "test_component": {
            "label": "//some/label/for/test:b",
            "moniker": "/core/testing/devices-tests"
          }
        }
        ]);
        let parsed: Vec<TestComponentsJsonEntry> = serde_json::from_value(json).unwrap();

        let lookup_map = TestComponentsJsonEntry::convert_to_map(parsed).unwrap();

        assert_eq!(
            lookup_map.get("//some/label/for/test:a"),
            Some("/core/testing/system-tests".into()).as_ref()
        );
        assert_eq!(
            lookup_map.get("//some/label/for/test:b"),
            Some("/core/testing/devices-tests".into()).as_ref()
        );
    }

    #[test]
    fn test_parse_test_components_json_entry_fails_on_duplicates() {
        let json = json!([
        {
          "test_component": {
            "label": "//some/label/for/test:a",
            "moniker": "/core/testing/system-tests"
          }
        },
        {
          "test_component": {
            "label": "//some/label/for/test:a",
            "moniker": "/core/testing/devices-tests"
          }
        }
        ]);
        let parsed: Vec<TestComponentsJsonEntry> = serde_json::from_value(json).unwrap();

        let result = TestComponentsJsonEntry::convert_to_map(parsed);
        assert!(result.is_err());
    }
}
