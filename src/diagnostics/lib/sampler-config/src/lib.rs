// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/42169836): Refactor these into a generic ValueList
mod selector_list;
mod string_list;

use anyhow::{bail, Context as _, Error};
use fuchsia_inspect as inspect;
use futures::FutureExt;
use glob::{GlobError, Paths};
use log::warn;
use moniker::ExtendedMoniker;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use string_list::StringList;

pub use selector_list::{ParsedSelector, SelectorList};

const MONIKER_INTERPOLATION: &str = "{MONIKER}";
const INSTANCE_ID_INTERPOLATION: &str = "{INSTANCE_ID}";
const METRICS_INSPECT_SIZE_BYTES: usize = 1024 * 1024; // 1MiB
const DEFAULT_MIN_SAMPLE_RATE_SEC: i64 = 10;
const INSTANCE_IDS_PATH: &str = "config/data/component_id_index";

/// Configuration for a single project to map Inspect data to its Cobalt metrics.
#[derive(Deserialize, Debug, PartialEq)]
pub struct ProjectConfig {
    /// Project ID that metrics are being sampled and forwarded on behalf of.
    pub project_id: u32,

    // Customer ID that metrics are being sampled and forwarded on behalf of.
    // This will default to 1 if not specified. Read it with the customer_id() function.
    customer_id: Option<u32>,

    /// The frequency with which metrics are sampled, in seconds.
    pub poll_rate_sec: i64,

    /// The collection of mappings from Inspect to Cobalt.
    pub metrics: Vec<MetricConfig>,

    /// File name the struct was loaded from
    #[serde(skip, default = "default_source_name")]
    source_name: String,
}

impl ProjectConfig {
    /// Customer ID that metrics are being sampled and forwarded on behalf of.
    /// This will default to 1 if not specified.
    pub fn customer_id(&self) -> u32 {
        self.customer_id.unwrap_or(1)
    }
}

/// Configuration for a single FIRE project template to map Inspect data to its Cobalt metrics
/// for all components in the ComponentIdInfo. Just like ProjectConfig except it uses MetricTemplate
/// instead of MetricConfig.
#[derive(Deserialize, Debug, PartialEq)]
struct ProjectTemplate {
    /// Project ID that metrics are being sampled and forwarded on behalf of.
    project_id: u32,

    /// Customer ID that metrics are being sampled and forwarded on behalf of.
    /// This will default to 1 if not specified.
    customer_id: Option<u32>,

    /// The frequency with which metrics are sampled, in seconds.
    poll_rate_sec: i64,

    /// The collection of mappings from Inspect to Cobalt.
    metrics: Vec<MetricTemplate>,

    /// File name the struct was loaded from
    #[serde(skip, default = "default_source_name")]
    source_name: String,
}

fn default_source_name() -> String {
    "<unknown>".to_string()
}

/// Configuration for a single metric to map from an Inspect property
/// to a Cobalt metric.
#[derive(Clone, Deserialize, Debug, PartialEq)]
pub struct MetricConfig {
    /// Selector identifying the metric to
    /// sample via the diagnostics platform.
    #[serde(rename = "selector")]
    pub selectors: SelectorList,
    /// Cobalt metric id to map the selector to.
    pub metric_id: u32,
    /// Data type to transform the metric to.
    pub metric_type: DataType,
    /// Event codes defining the dimensions of the
    /// Cobalt metric. Note: Order matters, and
    /// must match the order of the defined dimensions
    /// in the Cobalt metric file.
    /// Missing field means the same as empty list.
    #[serde(default)]
    pub event_codes: Vec<u32>,
    /// Optional boolean specifying whether to upload
    /// the specified metric only once, the first time
    /// it becomes available to the sampler.
    pub upload_once: Option<bool>,
    /// Optional project id. When present this project id will be used instead of the top-level
    /// project id.
    // TODO(https://fxbug.dev/42071858): remove this when we support batching.
    pub project_id: Option<u32>,
}

/// Configuration for a single FIRE metric template to map from an Inspect property
/// to a cobalt metric. Unlike MetricConfig, selectors aren't parsed.
#[derive(Clone, Deserialize, Debug, PartialEq)]
struct MetricTemplate {
    /// Selector identifying the metric to
    /// sample via the diagnostics platform.
    #[serde(rename = "selector")]
    selectors: StringList,
    /// Cobalt metric id to map the selector to.
    metric_id: u32,
    /// Data type to transform the metric to.
    metric_type: DataType,
    /// Event codes defining the dimensions of the
    /// cobalt metric.
    /// Notes:
    /// - Order matters, and must match the order of the defined dimensions
    ///   in the cobalt metric file.
    /// - The FIRE component-ID will be inserted as the first element of event_codes.
    /// - The event_codes field may be omitted from the config file if component-ID is the only
    ///   event code.
    event_codes: Option<Vec<u32>>,
    /// Optional boolean specifying whether to upload
    /// the specified metric only once, the first time
    /// it becomes available to the sampler.
    upload_once: Option<bool>,
    /// Optional project id. When present this project id will be used instead of the top-level
    /// project id.
    // TODO(https://fxbug.dev/42071858): remove this when we support batching.
    project_id: Option<u32>,
}

/// Supported Cobalt Metric types
#[derive(Deserialize, Debug, PartialEq, Eq, Copy, Clone)]
pub enum DataType {
    /// Maps cached diffs from Uint or Int Inspect types.
    /// NOTE: This does not use duration tracking. Durations
    ///       are always set to 0.
    Occurrence,
    /// Maps raw Int Inspect types.
    Integer,
    /// Maps cached diffs from IntHistogram Inspect type.
    IntHistogram,
    /// Maps Inspect String type to StringValue (Cobalt 1.1 only).
    String,
    // TODO(lukenicholson): Expand sampler support for new
    // data types.
    // Maps raw Double Inspect types.
    // FloatCustomEvent,
    // Maps raw Uint Inspect types.
    // IndexCustomEvent,
}

// #[cfg(test)] won't work because it's used outside the library.
pub fn parse_selector_for_test(selector_str: &str) -> Option<ParsedSelector> {
    Some(selector_list::parse_selector(selector_str).unwrap())
}

// TODO(https://fxbug.dev/42169836): Maybe refactor this into a generic ValueList - but remember that
// unlike StringList and SelectorList, it's not OK to have just a ComponentIdInfo in a config
// file - the file should always be a list even if there's just one (or zero) items.
#[derive(Deserialize, Debug)]
pub struct ComponentIdInfoList(Vec<ComponentIdInfo>);

#[derive(Deserialize, Debug)]
pub struct ComponentIdInfo {
    /// The component's moniker
    #[serde(deserialize_with = "moniker_deserialize")]
    moniker: ExtendedMoniker,
    /// The Component Instance ID - may not be available
    instance_id: Option<String>,
    /// The ID sent to Cobalt as an event code
    #[serde(alias = "id")]
    event_id: u32,
    /// Human-readable label, not used by Sampler, but we need to validate it
    #[allow(unused)]
    label: String,
}

fn moniker_deserialize<'de, D>(deserializer: D) -> Result<ExtendedMoniker, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let moniker_str = String::deserialize(deserializer)?;
    ExtendedMoniker::parse_str(&moniker_str).map_err(serde::de::Error::custom)
}

impl std::ops::Deref for ComponentIdInfoList {
    type Target = Vec<ComponentIdInfo>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ComponentIdInfoList {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl IntoIterator for ComponentIdInfoList {
    type Item = ComponentIdInfo;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

trait RemembersSource {
    fn remember_source(&mut self, _source: String);
}

impl RemembersSource for ProjectConfig {
    fn remember_source(&mut self, source: String) {
        self.source_name = source;
    }
}

impl RemembersSource for ProjectTemplate {
    fn remember_source(&mut self, source: String) {
        self.source_name = source;
    }
}

impl RemembersSource for ComponentIdInfoList {
    /// ComponentIdInfoList doesn't actually remember its source.
    fn remember_source(&mut self, _source: String) {}
}

fn paths_matching_name(path: impl AsRef<Path>, name: &str) -> Result<Paths, Error> {
    let path = path.as_ref();
    let pattern = path.join(name);
    Ok(glob::glob(&pattern.to_string_lossy())?)
}

fn load_many<T: DeserializeOwned + RemembersSource>(paths: Paths) -> Result<Vec<T>, Error> {
    paths
        .map(|path: Result<PathBuf, GlobError>| {
            let path = path?;
            let json_string: String =
                fs::read_to_string(&path).with_context(|| format!("parsing {}", path.display()))?;
            let mut config: T = serde_json5::from_str(&json_string)?;
            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(default_source_name);
            config.remember_source(file_name);
            Ok(config)
        })
        .collect::<Result<Vec<_>, _>>()
}

/// Container for all configurations needed to instantiate the Sampler infrastructure.
/// Includes:
///      - Project configurations.
///      - Whether to configure the ArchiveReader for tests (e.g. longer timeouts)
///      - Minimum sample rate.
#[derive(Debug)]
pub struct SamplerConfig {
    pub project_configs: Vec<Arc<ProjectConfig>>,
    pub configure_reader_for_tests: bool,
    pub minimum_sample_rate_sec: i64,

    // Used to store a lazy node that publishes data to Inspect if
    // present.
    inspect_node: Mutex<Option<inspect::LazyNode>>,
}

/// Use this struct in a builder pattern to load the Sampler, and
/// optionally FIRE, configs.
pub struct SamplerConfigBuilder {
    minimum_sample_rate_sec: i64,
    configure_reader_for_tests: bool,
    sampler_dir: Option<PathBuf>, // Not optional - load() will fail if this is not set.
    fire_dir: Option<PathBuf>,
}

impl Default for SamplerConfigBuilder {
    /// Call default() to start the builder.
    fn default() -> Self {
        SamplerConfigBuilder {
            minimum_sample_rate_sec: DEFAULT_MIN_SAMPLE_RATE_SEC,
            configure_reader_for_tests: false,
            sampler_dir: None,
            fire_dir: None,
        }
    }
}

impl SamplerConfigBuilder {
    /// Optional. If not called, a default value will be used.
    pub fn minimum_sample_rate_sec(mut self, minimum_sample_rate_sec: i64) -> Self {
        self.minimum_sample_rate_sec = minimum_sample_rate_sec;
        self
    }

    /// Optional. For use in tests only. Configures ArchiveReader
    /// (and thus ArchiveAccessor) to avoid test flakes.
    pub fn configure_reader_for_tests(mut self, configure_reader_for_tests: bool) -> Self {
        self.configure_reader_for_tests = configure_reader_for_tests;
        self
    }

    /// Required. The builder will fail without a Sampler config dir.
    /// Calling multiple times will use only the last value.
    pub fn sampler_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.sampler_dir = Some(path.into());
        self
    }

    /// Optional. FIRE config will be loaded if fire_dir() is called.
    /// Calling multiple times will use only the last value.
    pub fn fire_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.fire_dir = Some(path.into());
        self
    }

    /// Call load() after configuring the builder, to load and return SamplerConfig.
    pub fn load(self) -> Result<Arc<SamplerConfig>, Error> {
        if let Some(sampler_dir) = self.sampler_dir.as_ref() {
            SamplerConfig::from_directories_internal(
                self.minimum_sample_rate_sec,
                self.configure_reader_for_tests,
                sampler_dir,
                self.fire_dir,
            )
            .map(Arc::new)
        } else {
            bail!("sampler_dir must be configured before loading SamplerConfig")
        }
    }
}

impl MetricConfig {
    fn from_template(template: MetricTemplate, component: &ComponentIdInfo) -> Result<Self, Error> {
        let MetricTemplate {
            mut selectors,
            event_codes,
            metric_id,
            metric_type,
            upload_once,
            project_id,
        } = template;
        let selectors = SelectorList::from(
            selectors
                .iter_mut()
                .map::<Result<_, anyhow::Error>, _>(|s| {
                    match Self::interpolate_template(s, component) {
                        Ok(Some(filled_template)) => {
                            Ok(match selector_list::parse_selector(&filled_template) {
                                Ok(selector) => Ok(Some(selector)),
                                Err(err) => Err(err),
                            }?)
                        }
                        Ok(None) => Ok(None),
                        Err(err) => {
                            warn!(err:?; "Couldn't fill selector template");
                            Ok(None)
                        }
                    }
                })
                .collect::<Result<Vec<Option<_>>, _>>()?,
        );
        let event_codes = match event_codes {
            None => vec![component.event_id],
            Some(mut codes) => {
                codes.insert(0, component.event_id);
                codes
            }
        };
        Ok(MetricConfig { event_codes, selectors, metric_id, metric_type, upload_once, project_id })
    }

    /// Returns Ok(None) if the template needs a component ID and there's none available for
    /// the component. This is not an error and should be handled silently.
    fn interpolate_template(
        template: &str,
        component_info: &ComponentIdInfo,
    ) -> Result<Option<String>, Error> {
        let moniker_position = template.find(MONIKER_INTERPOLATION);
        let instance_id_position = template.find(INSTANCE_ID_INTERPOLATION);
        let separator_position = template.find(":");
        // If the insert position is before the first colon, it's the selector's moniker and
        // slashes should not be escaped.
        // Otherwise, treat the moniker string as a single Node or Property name,
        // and escape the appropriate characters.
        // Instance IDs have no special characters and don't need escaping.
        match (
            moniker_position,
            separator_position,
            instance_id_position,
            &component_info.instance_id,
        ) {
            (Some(i), Some(s), _, _) if i < s => Ok(Some(
                template.replace(MONIKER_INTERPOLATION, &component_info.moniker.to_string()),
            )),
            (Some(_), Some(_), _, _) => Ok(Some(template.replace(
                MONIKER_INTERPOLATION,
                &selectors::sanitize_string_for_selectors(&component_info.moniker.to_string()),
            ))),
            (_, _, Some(_), Some(id)) => Ok(Some(template.replace(INSTANCE_ID_INTERPOLATION, id))),
            (_, _, Some(_), None) => Ok(None),
            (None, _, None, _) => {
                bail!(
                    "{} and {} not found in selector template {}",
                    MONIKER_INTERPOLATION,
                    INSTANCE_ID_INTERPOLATION,
                    template
                )
            }
            (Some(_), None, _, _) => {
                bail!("Separator ':' not found in selector template {}", template)
            }
        }
    }
}

impl ProjectConfig {
    fn from_template(
        template: ProjectTemplate,
        components: &[ComponentIdInfo],
    ) -> Result<Self, Error> {
        let ProjectTemplate { metrics, customer_id, project_id, poll_rate_sec, source_name } =
            template;
        let mut expanded_metrics = vec![];
        for component in components.iter() {
            for metric in &metrics {
                expanded_metrics.push(MetricConfig::from_template(metric.to_owned(), component)?);
            }
        }
        Ok(ProjectConfig {
            metrics: expanded_metrics,
            customer_id,
            project_id,
            poll_rate_sec,
            source_name,
        })
    }
}

fn add_instance_ids(ids: component_id_index::Index, fire_components: &mut Vec<ComponentIdInfo>) {
    for component in fire_components {
        if let ExtendedMoniker::ComponentInstance(moniker) = &component.moniker {
            component.instance_id = ids.id_for_moniker(moniker).map(|h| format!("{h}"));
        }
    }
}

fn expand_fire_projects(
    projects: Vec<ProjectTemplate>,
    components: Vec<ComponentIdInfo>,
) -> Result<Vec<ProjectConfig>, Error> {
    projects
        .into_iter()
        .map(|project| ProjectConfig::from_template(project, &components))
        .collect::<Result<Vec<_>, _>>()
}

impl SamplerConfig {
    /// Parse the ProjectConfigurations for every project from config data.
    /// If a FIRE directory is given, load FIRE data and convert it to ProjectConfig's.
    fn from_directories_internal(
        minimum_sample_rate_sec: i64,
        configure_reader_for_tests: bool,
        sampler_dir: impl AsRef<Path>,
        fire_dir: Option<impl AsRef<Path>>,
    ) -> Result<Self, Error> {
        // TODO(https://fxbug.dev/42169894): Remove legacy_sampler_config_paths when
        // all config dirs use sampler_dir/foo/*.json
        let legacy_sampler_config_paths = paths_matching_name(&sampler_dir, "*.json")?;
        let sampler_config_paths = paths_matching_name(&sampler_dir, "*/*.json")?;
        let mut project_configs = load_many(sampler_config_paths)?;
        project_configs.append(&mut load_many(legacy_sampler_config_paths)?);
        if let Some(fire_dir) = fire_dir {
            let fire_project_paths = paths_matching_name(&fire_dir, "*/projects/*.json5")?;
            let fire_component_paths = paths_matching_name(&fire_dir, "*/components.json5")?;
            let fire_project_templates = load_many(fire_project_paths)?;
            let fire_components = load_many::<ComponentIdInfoList>(fire_component_paths)?;
            let mut fire_components =
                fire_components.into_iter().flatten().collect::<Vec<ComponentIdInfo>>();
            match component_id_index::Index::from_fidl_file(INSTANCE_IDS_PATH.into()) {
                Ok(ids) => add_instance_ids(ids, &mut fire_components),
                Err(error) => {
                    warn!(
                        error:?;
                        "Unable to read ID file; FIRE selectors with instance IDs won't work"
                    );
                }
            };
            project_configs
                .append(&mut expand_fire_projects(fire_project_templates, fire_components)?);
        }
        let project_configs = project_configs.into_iter().map(Arc::new).collect();
        Ok(Self {
            minimum_sample_rate_sec,
            project_configs,
            configure_reader_for_tests,
            inspect_node: Mutex::new(None),
        })
    }

    /// Publish data about this config to Inspect under the given node.
    ///
    /// Takes an Arc so that a weak reference can be used to populate an
    /// Inspector on demand.
    pub fn publish_inspect(self: &Arc<Self>, parent_node: &inspect::Node) {
        let weak_self = Arc::downgrade(self);
        let mut locked_node = self.inspect_node.lock().unwrap();
        *locked_node = Some(parent_node.create_lazy_child("metrics_sent", move || {
            let local_self = weak_self.upgrade();
            if local_self.is_none() {
                return async move { Ok(inspect::Inspector::default()) }.boxed();
            }

            let local_self = local_self.unwrap();

            let inspector = inspect::Inspector::new(
                inspect::InspectorConfig::default().size(METRICS_INSPECT_SIZE_BYTES),
            );
            let top_node = inspector.root();

            for config in local_self.project_configs.iter() {
                let mut next_selector_index = 0;
                // "<unknown>" should never happen, so it's better not to make it StringReference.
                let source_name = config.source_name.clone();
                top_node.record_child(source_name, |file_node| {
                    for metric in config.metrics.iter() {
                        for selector in metric.selectors.iter().flatten() {
                            file_node.record_child(
                                format!("{}", next_selector_index),
                                |selector_node| {
                                    next_selector_index += 1;
                                    selector_node.record_string(
                                        "selector",
                                        selector.selector_string.clone(),
                                    );
                                    selector_node
                                        .record_uint("upload_count", selector.get_upload_count());
                                },
                            );
                        }
                    }
                });
            }

            async move { Ok(inspector) }.boxed()
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::{MetricConfig, SamplerConfigBuilder};
    use crate::ComponentIdInfo;
    use component_id_index::InstanceId;
    use moniker::Moniker;
    use std::fs;

    #[fuchsia::test]
    fn parse_valid_sampler_configs() {
        let dir = tempfile::tempdir().unwrap();
        let load_path = dir.path();
        let config_path = load_path.join("config");
        fs::create_dir(&config_path).unwrap();
        fs::write(config_path.join("ok.json"), r#"{
  "project_id": 5,
  "poll_rate_sec": 60,
  "metrics": [
    {
      // Test comment for json5 portability.
      "selector": "bootstrap/archivist:root/all_archive_accessor:inspect_batch_iterator_get_next_requests",
      "metric_id": 1,
      "metric_type": "Occurrence",
      "event_codes": [0, 0]
    }
  ]
}
"#).unwrap();
        fs::write(config_path.join("ignored.txt"), "This file is ignored").unwrap();
        fs::write(
            config_path.join("also_ok.json"),
            r#"{
  "project_id": 5,
  "poll_rate_sec": 3,
  "metrics": [
    {
      "selector": "single_counter_test_component:root:counter",
      "metric_id": 1,
      "metric_type": "Occurrence",
      "event_codes": [0, 0]
    }
  ]
}
"#,
        )
        .unwrap();

        let config = SamplerConfigBuilder::default()
            .minimum_sample_rate_sec(10)
            .sampler_dir(load_path)
            .load();
        assert!(config.is_ok());
        assert_eq!(config.unwrap().project_configs.len(), 2);
    }

    #[fuchsia::test]
    fn parse_one_valid_one_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let load_path = dir.path();
        let config_path = load_path.join("config");
        fs::create_dir(&config_path).unwrap();
        fs::write(config_path.join("ok.json"), r#"{
  "project_id": 5,
  "poll_rate_sec": 60,
  "metrics": [
    {
      // Test comment for json5 portability.
      "selector": "bootstrap/archivist:root/all_archive_accessor:inspect_batch_iterator_get_next_requests",
      "metric_id": 1,
      "metric_type": "Occurrence",
      "event_codes": [0, 0]
    }
  ]
}
"#).unwrap();
        fs::write(config_path.join("ignored.txt"), "This file is ignored").unwrap();
        fs::write(
            config_path.join("invalid.json"),
            r#"{
  "project_id": 5,
  "poll_rate_sec": 3,
  "invalid_field": "bad bad bad"
}
"#,
        )
        .unwrap();

        let config = SamplerConfigBuilder::default()
            .minimum_sample_rate_sec(10)
            .sampler_dir(load_path)
            .load();
        assert!(config.is_err());
    }

    #[fuchsia::test]
    fn parse_optional_args() {
        let dir = tempfile::tempdir().unwrap();
        let load_path = dir.path();
        let config_path = load_path.join("config");
        fs::create_dir(&config_path).unwrap();
        fs::write(config_path.join("true.json"), r#"{
  "project_id": 5,
  "poll_rate_sec": 60,
  "metrics": [
    {
      // Test comment for json5 portability.
      "selector": "bootstrap/archivist:root/all_archive_accessor:inspect_batch_iterator_get_next_requests",
      "metric_id": 1,
      "metric_type": "Occurrence",
      "event_codes": [0, 0],
      "upload_once": true,
    }
  ]
}
"#).unwrap();

        fs::write(
            config_path.join("false.json"), r#"{
  "project_id": 5,
  "poll_rate_sec": 60,
  "metrics": [
    {
      // Test comment for json5 portability.
      "selector": "bootstrap/archivist:root/all_archive_accessor:inspect_batch_iterator_get_next_requests",
      "metric_id": 1,
      "metric_type": "Occurrence",
      "event_codes": [0, 0],
      "upload_once": false,
    }
  ]
}
"#).unwrap();

        let config = SamplerConfigBuilder::default()
            .minimum_sample_rate_sec(10)
            .sampler_dir(load_path)
            .load();
        assert!(config.is_ok());
        assert_eq!(config.unwrap().project_configs.len(), 2);
    }

    #[fuchsia::test]
    fn default_customer_id() {
        let dir = tempfile::tempdir().unwrap();
        let load_path = dir.path();
        let config_path = load_path.join("config");
        fs::create_dir(&config_path).unwrap();
        fs::write(config_path.join("1default.json"), r#"{
  "project_id": 5,
  "poll_rate_sec": 60,
  "metrics": [
    {
      "selector": "bootstrap/archivist:root/all_archive_accessor:inspect_batch_iterator_get_next_requests",
      "metric_id": 1,
      "metric_type": "Occurrence",
      "event_codes": [0, 0]
    }
  ]
}
"#).unwrap();
        fs::write(
            config_path.join("2with_customer_id.json"),
            r#"{
  "customer_id": 6,
  "project_id": 5,
  "poll_rate_sec": 3,
  "metrics": [
    {
      "selector": "single_counter_test_component:root:counter",
      "metric_id": 1,
      "metric_type": "Occurrence",
      "event_codes": [0, 0]
    }
  ]
}
"#,
        )
        .unwrap();

        let config = SamplerConfigBuilder::default()
            .minimum_sample_rate_sec(10)
            .sampler_dir(load_path)
            .load();
        assert!(config.is_ok());
        assert_eq!(config.as_ref().unwrap().project_configs.len(), 2);
        assert_eq!(config.as_ref().unwrap().project_configs[0].customer_id(), 1);
        assert_eq!(config.as_ref().unwrap().project_configs[1].customer_id(), 6);
    }

    #[fuchsia::test]
    fn missing_event_codes_ok() {
        let dir = tempfile::tempdir().unwrap();
        let load_path = dir.path();
        let config_path = load_path.join("config");
        fs::create_dir(&config_path).unwrap();
        fs::write(config_path.join("default.json"), r#"{
  "project_id": 5,
  "poll_rate_sec": 60,
  "metrics": [
    {
      "selector": "bootstrap/archivist:root/all_archive_accessor:inspect_batch_iterator_get_next_requests",
      "metric_id": 1,
      "metric_type": "Occurrence",
    }
  ]
}
"#).unwrap();
        let config = SamplerConfigBuilder::default()
            .minimum_sample_rate_sec(10)
            .sampler_dir(load_path)
            .load();
        assert!(config.is_ok());
        assert_eq!(config.as_ref().unwrap().project_configs[0].metrics[0].event_codes, vec![]);
    }

    #[fuchsia::test]
    fn fire_config_loading() {
        let sampler_dir = tempfile::tempdir().unwrap();
        let sampler_load_path = sampler_dir.path();
        let fire_dir = tempfile::tempdir().unwrap();
        let fire_load_path = fire_dir.path();
        let sampler_config_path = sampler_load_path.join("config");
        let fire_config_path_1 = fire_load_path.join("config1");
        let fire_config_path_2 = fire_load_path.join("config2");
        fs::create_dir(&sampler_config_path).unwrap();
        fs::create_dir(&fire_config_path_1).unwrap();
        fs::create_dir(&fire_config_path_2).unwrap();
        fs::write(
            sampler_config_path.join("some_name.json"),
            r#"{
            "project_id": 5,
            "customer_id": 6,
            "poll_rate_sec": 60,
            "metrics": [
                {
                "selector": "bootstrap/archivist:root/all_archive_accessor:requests",
                "metric_id": 1,
                "metric_type": "Occurrence",
                "event_codes": [0, 0],
                "project_id": 4
                }
            ]
            }
        "#,
        )
        .unwrap();
        fs::write(
            fire_config_path_1.join("components.json5"),
            r#"[
                {
                    "id": 42,
                    "label": "Foo_42",
                    "moniker": "core/foo42"
                }
            ]"#,
        )
        .unwrap();
        fs::write(
            fire_config_path_2.join("components.json5"),
            r#"[
                {
                    id: 43,
                    label: "Bar_43",
                    moniker: "bar43",
                },
            ]"#,
        )
        .unwrap();
        fs::create_dir(fire_config_path_1.join("projects")).unwrap();
        fs::write(
            fire_config_path_1.join("projects/some_name.json5"),
            r#"{
            "project_id": 13,
            "customer_id": 7,
            "poll_rate_sec": 60,
            "metrics": [
                {
                "selector": "{MONIKER}:root/path:leaf",
                "metric_id": 1,
                "metric_type": "Occurrence",
                "event_codes": [1, 2]
                }
            ]
            }
        "#,
        )
        .unwrap();
        fs::write(
            fire_config_path_1.join("projects/another_name.json5"),
            r#"{
            "project_id": 13,
            "poll_rate_sec": 60,
            "customer_id": 8,
            "metrics": [
                {
                "selector": [
                    "{MONIKER}:root/path2:leaf2",
                    "foo/bar:root/{MONIKER}:leaf3",
                    "asdf/qwer:root/path4:pre-{MONIKER}-post",
                ],
                "metric_id": 2,
                "metric_type": "Occurrence",
                }
            ]
            }
        "#,
        )
        .unwrap();

        let config = SamplerConfigBuilder::default()
            .minimum_sample_rate_sec(10)
            .sampler_dir(sampler_load_path)
            .fire_dir(fire_load_path)
            .load();
        assert!(config.is_ok());
        let configs = &config.as_ref().unwrap().project_configs;
        // Customer ID 6 is normal Sampler config. ID 7 and 8 are FIRE configs. There must be
        // one project config for each customer ID, 3 total.
        let config_6 = configs.iter().find(|c| c.customer_id() == 6).unwrap();
        let config_7 = configs.iter().find(|c| c.customer_id() == 7).unwrap();
        let config_8 = configs.iter().find(|c| c.customer_id() == 8).unwrap();
        let metric_6 = config_6.metrics.iter().find(|m| m.event_codes == vec![0, 0]).unwrap();
        let metric_7_42 =
            config_7.metrics.iter().find(|m| m.event_codes == vec![42, 1, 2]).unwrap();
        let metric_7_43 =
            config_7.metrics.iter().find(|m| m.event_codes == vec![43, 1, 2]).unwrap();
        let metric_8_42 = config_8.metrics.iter().find(|m| m.event_codes == vec![42]).unwrap();
        let metric_8_43 = config_8.metrics.iter().find(|m| m.event_codes == vec![43]).unwrap();

        // Make sure we don't have any extra configs or metrics
        assert_eq!(configs.len(), 3);
        assert_eq!(config_6.metrics.len(), 1);
        assert_eq!(config_7.metrics.len(), 2);
        assert_eq!(config_8.metrics.len(), 2);
        // Make sure all metrics have the right selectors
        assert_eq!(
            metric_6
                .selectors
                .iter()
                .map(|s| s.as_ref().unwrap().selector_string.to_owned())
                .collect::<Vec<_>>(),
            vec!["bootstrap/archivist:root/all_archive_accessor:requests"]
        );
        assert_eq!(metric_6.project_id, Some(4));
        assert_eq!(
            metric_7_42
                .selectors
                .iter()
                .map(|s| s.as_ref().unwrap().selector_string.to_owned())
                .collect::<Vec<_>>(),
            vec!["core/foo42:root/path:leaf"]
        );
        assert_eq!(
            metric_7_43
                .selectors
                .iter()
                .map(|s| s.as_ref().unwrap().selector_string.to_owned())
                .collect::<Vec<_>>(),
            vec!["bar43:root/path:leaf"]
        );
        assert_eq!(
            metric_8_42
                .selectors
                .iter()
                .map(|s| s.as_ref().unwrap().selector_string.to_owned())
                .collect::<Vec<_>>(),
            vec![
                "core/foo42:root/path2:leaf2",
                "foo/bar:root/core\\/foo42:leaf3",
                "asdf/qwer:root/path4:pre-core\\/foo42-post",
            ]
        );
        assert_eq!(
            metric_8_43
                .selectors
                .iter()
                .map(|s| s.as_ref().unwrap().selector_string.to_owned())
                .collect::<Vec<_>>(),
            vec![
                "bar43:root/path2:leaf2",
                "foo/bar:root/bar43:leaf3",
                "asdf/qwer:root/path4:pre-bar43-post",
            ]
        );
    }

    #[fuchsia::test]
    fn index_substitution_works() {
        let mut ids = component_id_index::Index::default();
        let foo_bar_moniker = Moniker::parse_str("foo/bar").unwrap();
        let qwer_asdf_moniker = Moniker::parse_str("qwer/asdf").unwrap();
        ids.insert(
            foo_bar_moniker,
            "1234123412341234123412341234123412341234123412341234123412341234"
                .parse::<InstanceId>()
                .unwrap(),
        )
        .unwrap();
        ids.insert(
            qwer_asdf_moniker,
            "1234abcd1234abcd123412341234123412341234123412341234123412341234"
                .parse::<InstanceId>()
                .unwrap(),
        )
        .unwrap();
        let mut components = vec![
            ComponentIdInfo {
                moniker: "baz/quux".try_into().unwrap(),
                event_id: 101,
                label: "bq".into(),
                instance_id: None,
            },
            ComponentIdInfo {
                moniker: "foo/bar".try_into().unwrap(),
                event_id: 102,
                label: "fb".into(),
                instance_id: None,
            },
        ];
        super::add_instance_ids(ids, &mut components);
        let moniker_template = "fizz/buzz:root/info/{MONIKER}/data";
        let id_template = "fizz/buzz:root/info/{INSTANCE_ID}/data";
        assert_eq!(
            MetricConfig::interpolate_template(moniker_template, &components[0]).unwrap().unwrap(),
            "fizz/buzz:root/info/baz\\/quux/data".to_string()
        );
        assert_eq!(
            MetricConfig::interpolate_template(moniker_template, &components[1]).unwrap().unwrap(),
            "fizz/buzz:root/info/foo\\/bar/data".to_string()
        );
        assert_eq!(MetricConfig::interpolate_template(id_template, &components[0]).unwrap(), None);
        assert_eq!(
            MetricConfig::interpolate_template(id_template, &components[1]).unwrap().unwrap(),
            "fizz/buzz:root/info/1234123412341234123412341234123412341234123412341234123412341234/data".to_string()
        );
    }
}
