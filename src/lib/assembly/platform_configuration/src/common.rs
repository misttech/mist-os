// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::collections::{btree_map::Entry, BTreeSet};

use assembly_config_schema::{BoardInformation, BuildType, FileEntry};
use assembly_util::NamedMap;

/// The platform's base service level.
///
/// This is the basis for the contract with the product as to what the minimal
/// set of services that are available in the platform will be.  Features can
/// be enabled on top of this most-basic level, but some features will require
/// a higher basic level of support.
///
/// These were initially based on the product definitions that are used to
/// provide the basis for all other products:
///
/// bringup.gni  (Bootstrap)
///   +--> minimal.gni  (Minimal)
///         +--> core.gni
///               +--> (everything else)
///
/// The `Utility` level is between `Bootstrap` and `Minimal`, adding the `/core`
/// realm and those children of `/core` needed by all systems that include
/// `/core`.
///
/// The standard, default, level is `Minimal`, and is the level that should be
/// used by products' main system.
///
/// Note:  This version of the enum does not contain the
/// `assembly_config_schema::FeatureSetLevel::Empty` option, as that is instead
/// represented as `Option::None`, with the other values as an
/// `Option::Some(value)`.
#[derive(Debug, PartialEq)]
pub(crate) enum FeatureSupportLevel {
    /// Bootable, but serial-only.  This is only the `/bootstrap` realm.  No
    /// netstack, no storage drivers, etc.  this is the smallest bootable system
    /// created by assembly, and is primarily used for board-level bringup.
    ///
    /// https://fuchsia.dev/fuchsia-src/development/build/build_system/bringup
    Bootstrap,

    /// This is the smallest configuration that includes the `/core` realm, and
    /// is best suited for utility-type systems such as recovery.  The "main"
    /// system for a product should not use this, and instead use the default.
    Utility,

    /// This is the smallest "full Fuchsia" configuration.  This has a netstack,
    /// can update itself, and has all the subsystems that are required to
    /// ship a production-level product.
    ///
    /// This is the default level unless otherwise specified.
    Minimal,
}
impl FeatureSupportLevel {
    /// Convert a deserialized assembly_config_schema:: FeatureSetLevel into an
    /// `Option<FeatureSetLevel>`, where the `Empty` case becomes `None`.
    pub fn from_deserialized(
        value: &assembly_config_schema::platform_config::FeatureSupportLevel,
    ) -> Option<Self> {
        match value {
            assembly_config_schema::FeatureSupportLevel::Empty => None,
            assembly_config_schema::FeatureSupportLevel::Bootstrap => {
                Some(FeatureSupportLevel::Bootstrap)
            }
            assembly_config_schema::FeatureSupportLevel::Utility => {
                Some(FeatureSupportLevel::Utility)
            }
            assembly_config_schema::FeatureSupportLevel::Minimal => {
                Some(FeatureSupportLevel::Minimal)
            }
        }
    }
}

/// A trait for subsystems to implement to provide the configuration for their
/// subsystem's components.
pub(crate) trait DefineSubsystemConfiguration<T> {
    /// Given the feature_set_level and build_type, along with the configuration
    /// schema for the subsystem, add its configuration to the builder.
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        subsystem_config: &T,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()>;
}

/// This provides the context that's passed to each subsystem's configuration
/// module.  These are the fields from the
/// `assembly_config_schema::platform_config::PlatformConfig` struct that are
/// available to all subsystems to use to derive their configuration from.
pub(crate) struct ConfigurationContext<'a> {
    pub feature_set_level: &'a FeatureSupportLevel,
    pub build_type: &'a BuildType,
    pub board_info: Option<&'a BoardInformation>,
}

/// A struct for collecting multiple kinds of platform configuration.
///
/// Subsystem configuration structs use this builder to add their configuration
/// to the assembly.
///
/// Usage:
/// ```
/// use crate::subsystems::prelude::*;
/// let builder = ConfigurationBuilder::default();
/// builder.platform_bundle("wlan")?;
///
/// // to set a single field on a single component in a package:
/// builder.package("wlancfg").component("wlancfg").field("my_key", "some_value");
///
/// // to set a single field on multiple components in the same package:
/// let mut swd_package = builder.package("swd");
/// swd_package.component("meta/foo.cm").field("some_key", "some_value");
/// swd_package.component("meta/bar.cm").field("some_key", "some_value");
///
/// // to set multiple fieds on the same component:
/// swd_package.component("meta/mine.cm")
///   .field("a", "value1")
///   .field("b", "value2");
/// ```
///
pub(crate) trait ConfigurationBuilder {
    /// Add a platform assembly input bundle that should be included in the
    /// assembled platform.
    fn platform_bundle(&mut self, name: &str);

    /// Add configuration for items in bootfs.
    fn bootfs(&mut self) -> &mut dyn BootfsConfigBuilder;

    /// Add configuration for a named package in one of the package sets.
    fn package(&mut self, name: &str) -> &mut dyn PackageConfigBuilder;

    /// Create a new domain config package.
    fn add_domain_config(&mut self, name: &str) -> &mut dyn DomainConfigBuilder;
}

/// The interface for specifying the configuration to provide for bootfs.
pub(crate) trait BootfsConfigBuilder {
    /// Add configuration to the builder for a component within a package.
    fn component(&mut self, pkg_path: &str) -> Result<&mut dyn ComponentConfigBuilder>;
}

/// The interface for specifying the configuration to provide for a package.
pub(crate) trait PackageConfigBuilder {
    /// Add configuration to the builder for a component within a package.
    fn component(&mut self, pkg_path: &str) -> Result<&mut dyn ComponentConfigBuilder>;

    /// Add a config data file to the package in the builder.
    fn config_data(&mut self, file_entry: FileEntry) -> Result<&mut dyn PackageConfigBuilder>;
}

/// The interface for building a domain config.
pub(crate) trait DomainConfigBuilder {
    /// Add a directory to the domain config which can hold config resources.
    fn directory(&mut self, name: &str) -> &mut dyn DomainConfigDirectoryBuilder;
}

/// The interface for specifying the config files to add to a domain config package directory.
pub(crate) trait DomainConfigDirectoryBuilder {
    fn entry(&mut self, file_entry: FileEntry) -> Result<&mut dyn DomainConfigDirectoryBuilder>;
}

/// The interface for specifying the configuration to provide for a component.
pub(crate) trait ComponentConfigBuilder {
    /// Add a value for a Structured Configuration field for a given component.
    fn field_value(
        &mut self,
        key: &str,
        value: serde_json::Value,
    ) -> Result<&mut dyn ComponentConfigBuilder>;
}

/// An extension trait that allows the field value to be passed without calling
/// `.into()` at the call-site.
pub(crate) trait ComponentConfigBuilderExt {
    /// Add a value for a Structured Configuration field for a given component.
    fn field(
        &mut self,
        key: &str,
        value: impl Into<serde_json::Value>,
    ) -> Result<&mut dyn ComponentConfigBuilder>;
}

impl ComponentConfigBuilderExt for &mut dyn ComponentConfigBuilder {
    fn field(
        &mut self,
        key: &str,
        value: impl Into<serde_json::Value>,
    ) -> Result<&mut dyn ComponentConfigBuilder> {
        ComponentConfigBuilder::field_value(*self, key, value.into())
    }
}

/// The in-progress builder, which hides its state.
pub(crate) struct ConfigurationBuilderImpl {
    /// The Assembly Input Bundles to add.
    bundles: BTreeSet<String>,

    /// BootFS configuration.
    bootfs: BootfsConfig,

    /// Per-package configuration.
    package_configs: PackageConfigs,

    /// The domain config packages to add.
    domain_configs: DomainConfigs,
}

impl Default for ConfigurationBuilderImpl {
    fn default() -> Self {
        Self {
            bundles: BTreeSet::default(),
            bootfs: BootfsConfig::default(),
            package_configs: PackageConfigs::new("package configs"),
            domain_configs: DomainConfigs::new("domain configs"),
        }
    }
}

impl ConfigurationBuilderImpl {
    /// Convert the builder into the completed configuration that can be used
    /// to create the configured platform itself.
    pub fn build(self) -> CompletedConfiguration {
        let Self { bundles, bootfs, package_configs, domain_configs } = self;
        CompletedConfiguration { bundles, bootfs, package_configs, domain_configs }
    }
}

/// The struct containing the resultant configuration to apply to the platform.
pub struct CompletedConfiguration {
    /// The list of the Platform Assembly Input Bundles to add.
    ///
    /// This is a list of Platform AIBs by name (not path).
    ///
    pub bundles: BTreeSet<String>,

    /// Configuration for items in bootfs.
    pub bootfs: BootfsConfig,

    /// Per-package configuration for named packages in the package sets
    ///
    /// Which set doesn't matter, as a package can only be in one package set in
    /// the assembled image.
    pub package_configs: PackageConfigs,

    /// The list of domain configs to add.
    pub domain_configs: DomainConfigs,
}

/// A map from package names to the configuration to apply to them.
pub type PackageConfigs = NamedMap<PackageConfiguration>;

/// A map from component manifest path with a namespace to the values for for the component.
pub type ComponentConfigs = NamedMap<ComponentConfiguration>;

/// A map from package name to domain config.
pub type DomainConfigs = NamedMap<DomainConfig>;

/// All of the configuration that applies to a single package.
///
/// This holds:
/// - config_data entries for the package
/// - for each component:
///     - Structured Config
///
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PackageConfiguration {
    /// A map from manifest paths within the package namespace to the values for the component.
    pub components: ComponentConfigs,

    /// A map of config data entries, keyed by the destination path.
    pub config_data: NamedMap<FileEntry>,

    /// The package name.
    pub name: String,
}

impl PackageConfiguration {
    /// Construt a new PackageConfiguration.
    pub fn new(name: impl AsRef<str>) -> Self {
        PackageConfiguration {
            components: ComponentConfigs::new("component configs"),
            config_data: NamedMap::new("config data"),
            name: name.as_ref().into(),
        }
    }
}

/// All of the configuration for a single component.
///
/// This holds:
/// - Structured Config values for this component.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ComponentConfiguration {
    /// Structured Config key-value pairs.
    pub fields: NamedMap<serde_json::Value>,

    /// The component's manifest path in its package or in bootfs
    manifest_path: String,
}

impl Default for ComponentConfiguration {
    fn default() -> Self {
        Self { fields: NamedMap::new("structured config fields"), manifest_path: String::default() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DomainConfig {
    pub directories: NamedMap<DomainConfigDirectory>,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DomainConfigDirectory {
    pub entries: NamedMap<FileEntry>,
}

impl ConfigurationBuilder for ConfigurationBuilderImpl {
    fn platform_bundle(&mut self, name: &str) {
        self.bundles.insert(name.to_string());
    }

    fn bootfs(&mut self) -> &mut dyn BootfsConfigBuilder {
        &mut self.bootfs
    }

    fn package(&mut self, name: &str) -> &mut dyn PackageConfigBuilder {
        self.package_configs.entry(name.to_string()).or_insert_with_key(|name| {
            PackageConfiguration {
                components: ComponentConfigs::new("component configs"),
                config_data: NamedMap::new("config data"),
                name: name.to_owned(),
            }
        })
    }

    fn add_domain_config(&mut self, name: &str) -> &mut dyn DomainConfigBuilder {
        self.domain_configs.entry(name.to_string()).or_insert_with_key(|name| DomainConfig {
            directories: NamedMap::new("directories"),
            name: name.to_owned(),
        })
    }
}

impl DomainConfigBuilder for DomainConfig {
    fn directory(&mut self, name: &str) -> &mut dyn DomainConfigDirectoryBuilder {
        self.directories
            .entry(name.to_string())
            .or_insert_with(|| DomainConfigDirectory { entries: NamedMap::new("domain configs") })
    }
}

impl DomainConfigDirectoryBuilder for DomainConfigDirectory {
    fn entry(&mut self, file_entry: FileEntry) -> Result<&mut dyn DomainConfigDirectoryBuilder> {
        self.entries
            .try_insert_unique(file_entry.destination.clone(), file_entry)
            .context("A config destination can only be set once for a domain config")?;
        Ok(self)
    }
}

impl PackageConfigBuilder for PackageConfiguration {
    fn component(&mut self, pkg_path: &str) -> Result<&mut dyn ComponentConfigBuilder> {
        match self.components.entry(pkg_path.to_owned()) {
            entry @ Entry::Vacant(_) => {
                Ok(entry.or_insert_with_key(|path_in_package| ComponentConfiguration {
                    fields: NamedMap::new("structured config fields"),
                    manifest_path: path_in_package.to_owned(),
                }))
            }
            Entry::Occupied(_) => {
                Err(anyhow!("Each component's configuration can only be set once"))
                    .with_context(|| format!("Setting configuration for component: {pkg_path}"))
                    .with_context(|| anyhow!("Setting configuration for package: {}", self.name))
            }
        }
    }

    fn config_data(&mut self, file_entry: FileEntry) -> Result<&mut dyn PackageConfigBuilder> {
        self.config_data
            .try_insert_unique(file_entry.destination.clone(), file_entry)
            .context("A config data destination can only be set once for a package")?;
        Ok(self)
    }
}

impl ComponentConfigBuilder for ComponentConfiguration {
    /// Add a value for a Structured Configuration field for a given component.
    fn field_value(
        &mut self,
        key: &str,
        value: serde_json::Value,
    ) -> Result<&mut dyn ComponentConfigBuilder> {
        self.fields
            .try_insert_unique(key.to_owned(), value)
            .context("Each Structured Config field can only be set once for a component")?;
        Ok(self)
    }
}

/// Configuration of components in bootfs.
///
/// This is separate from PackageConfig because it may have to place bare files
/// in bootfs.
#[derive(Clone, Debug, PartialEq)]
pub struct BootfsConfig {
    /// A map from manifest paths within bootfs to the configuration values for
    /// the component.
    pub components: ComponentConfigs,
}

impl Default for BootfsConfig {
    fn default() -> Self {
        Self { components: ComponentConfigs::new("component configs") }
    }
}

impl BootfsConfigBuilder for BootfsConfig {
    /// Add configuration to the builder for a component within bootfs.
    fn component(
        &mut self,
        component_manifest_path: &str,
    ) -> Result<&mut dyn ComponentConfigBuilder> {
        match self.components.entry(component_manifest_path.to_owned()) {
            entry @ Entry::Vacant(_) => {
                Ok(entry.or_insert_with_key(|component_manifest_path| ComponentConfiguration {
                    fields: NamedMap::new("structured config fields"),
                    manifest_path: component_manifest_path.to_owned(),
                }))
            }
            Entry::Occupied(_) => {
                Err(anyhow!("Each component's configuration can only be set once"))
                    .with_context(|| {
                        format!("Setting configuration for component: {component_manifest_path}")
                    })
                    .context("Setting configuration in bootfs")
            }
        }
    }
}

pub(crate) trait BoardInformationExt {
    /// Returns whether or not this board provides the named feature.
    fn provides_feature(&self, name: impl AsRef<str>) -> bool;
}

impl BoardInformationExt for BoardInformation {
    /// Returns whether or not this board provides the named feature.
    fn provides_feature(&self, name: impl AsRef<str>) -> bool {
        // .contains(&str) doesn't work for Vec<String>, so it's neccessary
        // to use .iter().any(...) instead.
        let name = name.as_ref();
        self.provided_features.iter().any(|f| f == name)
    }
}

impl BoardInformationExt for Option<&BoardInformation> {
    fn provides_feature(&self, name: impl AsRef<str>) -> bool {
        match self {
            Some(board_info) => board_info.provides_feature(name),
            _ => false,
        }
    }
}

/// DefaultByBuildType trait is implemented on each enum value to instantiate an
/// unconfigured update checker. Specifically, an Eng build-type will result in an
/// unconfigured system-update-checker and the User or UserDebug build-types will
/// result in an unconfigured omaha-client
///
/// ```
/// impl DefaultByBuildType for PolicyLabels {
///    fn default_by_build_type(build_type: &BuildType) -> Self {
///        match build_type {
///            BuildType::Eng => PolicyLabels::Unrestricted,
///            BuildType::UserDebug => PolicyLabels::LocalDynamicConfig,
///            BuildType::User => PolicyLabels::BaseComponentsOnly,
///        }
///    }
/// }
///
/// let policy = PolicyLabels::default_by_build_type(&BuildType::Eng);
/// assert_eq!(policy, PolicyLabels::Unrestricted);
/// ```
pub(crate) trait DefaultByBuildType {
    fn default_by_build_type(build_type: &BuildType) -> Self;
}

/// A trait which declares that a type T implements a fn which returns an instance of type T by
/// default (with respect to the build_type) or because the T struct has already been instantiated
pub(crate) trait OptionDefaultByBuildTypeExt<T: DefaultByBuildType> {
    fn value_or_default_from_build_type(self, build_type: &BuildType) -> T;
}

/// Returns an unwrapped instance of T if T is provided, else defers to T::default_by_build_type
/// Used in situations where the configuration value's default is dependent on the build_type,
/// and may not be provided by the product owner. Therefore, None ends up being converted to
/// a default which depends on the provided BuildType.
///
/// Usage:
/// ```
/// let config = SwdConfig {
///    policy: None,
///    ...
/// };
/// let policy = config.policy.value_or_default_from_build_type(&BuildType::Eng);
/// assert_eq!(policy, PolicyLabels::Unrestricted);
/// ```
///
///
///
impl<T: DefaultByBuildType> OptionDefaultByBuildTypeExt<T> for Option<T> {
    fn value_or_default_from_build_type(self, build_type: &BuildType) -> T {
        self.unwrap_or_else(|| T::default_by_build_type(build_type))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let mut builder = ConfigurationBuilderImpl::default();

        // using an inner
        fn make_config(builder: &mut dyn ConfigurationBuilder) -> Result<()> {
            builder.bootfs().component("some/bootfs_component")?.field("key", "value")?;

            builder
                .package("package_a")
                .component("meta/component_a1")?
                .field("key_a1", "value_a1")?;
            builder
                .package("package_a")
                .component("meta/component_a2")?
                .field("key_a2", "value_a2")?;
            builder
                .package("package_b")
                .component("meta/component_b")?
                .field("key_b1", "value_b1")?
                .field("key_b2", "value_b2")?;

            builder.package("package_a").config_data(FileEntry {
                destination: "config/one".into(),
                source: "config_data1".into(),
            })?;
            builder.package("package_a").config_data(FileEntry {
                destination: "config/two".into(),
                source: "config_data2".into(),
            })?;
            builder.package("package_b").config_data(FileEntry {
                destination: "config/one".into(),
                source: "config_data1".into(),
            })?;

            Ok(())
        }
        assert!(make_config(&mut builder).is_ok());
        let config = builder.build();

        assert_eq!(config.bootfs.components.len(), 1);

        assert_eq!(
            config.bootfs.components.get("some/bootfs_component").unwrap().fields,
            NamedMap {
                name: "structured config fields".into(),
                entries: [("key".into(), "value".into())].into(),
            },
        );
        assert_eq!(config.package_configs.len(), 2);
        assert_eq!(
            config.package_configs.get("package_a").unwrap(),
            &PackageConfiguration {
                name: "package_a".into(),
                components: NamedMap {
                    name: "component configs".into(),
                    entries: [
                        (
                            "meta/component_a1".into(),
                            ComponentConfiguration {
                                manifest_path: "meta/component_a1".into(),
                                fields: NamedMap {
                                    name: "structured config fields".into(),
                                    entries: [("key_a1".into(), "value_a1".into())].into()
                                },
                            }
                        ),
                        (
                            "meta/component_a2".into(),
                            ComponentConfiguration {
                                manifest_path: "meta/component_a2".into(),
                                fields: NamedMap {
                                    name: "structured config fields".into(),
                                    entries: [("key_a2".into(), "value_a2".into())].into(),
                                },
                            }
                        ),
                    ]
                    .into(),
                },
                config_data: NamedMap {
                    name: "config data".into(),
                    entries: [
                        (
                            "config/one".into(),
                            FileEntry {
                                destination: "config/one".into(),
                                source: "config_data1".into(),
                            }
                        ),
                        (
                            "config/two".into(),
                            FileEntry {
                                destination: "config/two".into(),
                                source: "config_data2".into(),
                            }
                        ),
                    ]
                    .into(),
                },
            }
        );
        assert_eq!(
            config.package_configs.get("package_b").unwrap(),
            &PackageConfiguration {
                name: "package_b".into(),
                components: NamedMap {
                    name: "component configs".into(),
                    entries: [(
                        "meta/component_b".into(),
                        ComponentConfiguration {
                            manifest_path: "meta/component_b".into(),
                            fields: NamedMap {
                                name: "structured config fields".into(),
                                entries: [
                                    ("key_b1".into(), "value_b1".into()),
                                    ("key_b2".into(), "value_b2".into())
                                ]
                                .into(),
                            },
                        }
                    )]
                    .into(),
                },
                config_data: NamedMap {
                    name: "config data".into(),
                    entries: [(
                        "config/one".into(),
                        FileEntry {
                            destination: "config/one".into(),
                            source: "config_data1".into(),
                        }
                    )]
                    .into(),
                },
            }
        );
    }

    #[test]
    fn test_multiple_adds_fail() {
        let mut builder = ConfigurationBuilderImpl::default();

        assert!(builder.bootfs().component("foo").is_ok());
        assert!(builder.bootfs().component("foo").is_err());

        assert!(builder.package("foo").component("bar").is_ok());
        assert!(builder.package("foo").component("bar").is_err());

        let mut component = builder.package("foo").component("baz").unwrap();
        assert!(component.field("key", "value").is_ok());
        assert!(component.field("key", "diff_value").is_err());
        assert!(component.field("key2", "value2").is_ok());
        assert!(component.field("key2", "value2").is_err());

        assert!(builder
            .package("foo")
            .config_data(FileEntry { destination: "bar".into(), source: "baz".into() })
            .is_ok());
        assert!(builder
            .package("foo")
            .config_data(FileEntry { destination: "cat".into(), source: "baz".into() })
            .is_ok());
        assert!(builder
            .package("foo")
            .config_data(FileEntry { destination: "cat".into(), source: "diz".into() })
            .is_err());
    }

    #[test]
    fn test_error_messages_for_multiple_adds() {
        // This test validates that the error messages produced by the builder,
        // when their context() entries are flattened, produces sensical-looking
        // errors.

        fn format_result<T>(result: Result<T>) -> String {
            if let Err(e) = result {
                format!(
                    "{} Failed{}",
                    e,
                    e.chain()
                        .skip(1)
                        .enumerate()
                        .map(|(i, e)| format!("\n  {: >3}.  {}", i + 1, e))
                        .collect::<Vec<String>>()
                        .concat()
                )
            } else {
                "Not An error".into()
            }
        }

        let mut builder = ConfigurationBuilderImpl::default();

        builder.bootfs().component("foo").unwrap();
        let result = builder.bootfs().component("foo").context("Configuring Subsystem");

        assert_eq!(
            format_result(result),
            r"Configuring Subsystem Failed
    1.  Setting configuration in bootfs
    2.  Setting configuration for component: foo
    3.  Each component's configuration can only be set once"
        );

        builder.package("foo").component("bar").unwrap();
        let result = builder.package("foo").component("bar").context("Configuring Subsystem");

        assert_eq!(
            format_result(result),
            r"Configuring Subsystem Failed
    1.  Setting configuration for package: foo
    2.  Setting configuration for component: bar
    3.  Each component's configuration can only be set once"
        );

        let mut component = builder.package("other").component("bar").unwrap();
        component.field("key", "value").unwrap();
        let result = component.field("key", "value2").context("Configuring Subsystem");

        assert_eq!(
            format_result(result),
            r#"Configuring Subsystem Failed
    1.  Each Structured Config field can only be set once for a component
    2.  duplicate entry in structured config fields:
  key: 'key'
  existing value: String("value")
  new value: String("value2")"#
        );
    }

    #[test]
    fn test_provides_feature() {
        let board_info = BoardInformation {
            name: "sample".to_owned(),
            provided_features: vec!["feature_a".into(), "feature_b".into()],
        };

        assert!(board_info.provides_feature("feature_a"));
        assert!(board_info.provides_feature("feature_b"));
        assert!(!board_info.provides_feature("feature_c"));
    }
}
