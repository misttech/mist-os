// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::{CustomerId, EventCode, MetricId, MetricType, ProjectId};
use anyhow::bail;
use component_id_index::InstanceId;
use log::warn;
use moniker::ExtendedMoniker;
use serde::{de, Deserialize, Deserializer};
use std::str::FromStr;

// At the moment there's no difference between the user facing project config and the one loaded at
// runtime. This will change as we integrate Cobalt mappings.
pub use crate::runtime::{MetricConfig, ProjectConfig};

/// Configuration for a single FIRE project template to map Inspect data to its Cobalt metrics
/// for all components in the ComponentIdInfo. Just like ProjectConfig except it uses MetricTemplate
/// instead of MetricConfig.
#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ProjectTemplate {
    /// Project ID that metrics are being sampled and forwarded on behalf of.
    pub project_id: ProjectId,

    /// Customer ID that metrics are being sampled and forwarded on behalf of.
    /// This will default to 1 if not specified.
    #[serde(default)]
    pub customer_id: CustomerId,

    /// The frequency with which metrics are sampled, in seconds.
    #[serde(deserialize_with = "crate::utils::greater_than_zero")]
    pub poll_rate_sec: i64,

    /// The collection of mappings from Inspect to Cobalt.
    pub metrics: Vec<MetricTemplate>,
}

impl ProjectTemplate {
    pub fn expand(self, components: &[ComponentIdInfo]) -> ProjectConfig {
        let ProjectTemplate { project_id, customer_id, poll_rate_sec, metrics } = self;
        let mut metric_configs = Vec::with_capacity(metrics.len() * components.len());
        for component in components {
            for metric in metrics.iter() {
                if let Some(metric_config) = metric.expand(component) {
                    metric_configs.push(metric_config);
                }
            }
        }
        ProjectConfig { project_id, customer_id, poll_rate_sec, metrics: metric_configs }
    }
}

/// Configuration for a single FIRE metric template to map from an Inspect property
/// to a cobalt metric. Unlike MetricConfig, selectors aren't parsed.
#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
pub struct MetricTemplate {
    /// Selector identifying the metric to
    /// sample via the diagnostics platform.
    #[serde(rename = "selector", deserialize_with = "crate::utils::one_or_many_strings")]
    pub selectors: Vec<String>,

    /// Cobalt metric id to map the selector to.
    pub metric_id: MetricId,

    /// Data type to transform the metric to.
    pub metric_type: MetricType,

    /// Event codes defining the dimensions of the
    /// cobalt metric.
    /// Notes:
    /// - Order matters, and must match the order of the defined dimensions
    ///   in the cobalt metric file.
    /// - The FIRE component-ID will be inserted as the first element of event_codes.
    /// - The event_codes field may be omitted from the config file if component-ID is the only
    ///   event code.
    #[serde(default)]
    pub event_codes: Vec<EventCode>,

    /// Optional boolean specifying whether to upload the specified metric only once, the first time
    /// it becomes available to the sampler. Defaults to false.
    #[serde(default)]
    pub upload_once: bool,

    /// Optional project id. When present this project id will be used instead of the top-level
    /// project id.
    // TODO(https://fxbug.dev/42071858): remove this when we support batching.
    pub project_id: Option<ProjectId>,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ComponentIdInfoList(Vec<ComponentIdInfo>);

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ComponentIdInfo {
    /// The component's moniker
    #[serde(deserialize_with = "moniker_deserialize")]
    pub moniker: ExtendedMoniker,

    /// The Component Instance ID - may not be available
    #[serde(default, deserialize_with = "instance_id_deserialize")]
    pub instance_id: Option<InstanceId>,

    /// The ID sent to Cobalt as an event code
    #[serde(alias = "id")]
    pub event_id: EventCode,

    /// Human-readable label, not used by Sampler, only for human configs.
    pub label: String,
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

impl MetricTemplate {
    fn expand(&self, component: &ComponentIdInfo) -> Option<MetricConfig> {
        let MetricTemplate {
            selectors,
            metric_id,
            metric_type,
            event_codes,
            upload_once,
            project_id,
        } = self;
        let mut selectors = selectors
            .iter()
            .filter_map(|s| {
                // TODO: forward error.
                match interpolate_template(s, component) {
                    // TODO: parse the selector here.
                    Ok(result) => result,
                    Err(err) => {
                        warn!(err:?, template=s.as_str(); "Couldn't fill selector template");
                        None
                    }
                }
            })
            .peekable();
        selectors.peek()?;
        let selectors = selectors.collect::<Vec<String>>();
        let mut event_codes = event_codes.to_vec();
        event_codes.insert(0, component.event_id);
        Some(MetricConfig {
            event_codes,
            selectors,
            metric_id: *metric_id,
            metric_type: *metric_type,
            upload_once: *upload_once,
            project_id: *project_id,
        })
    }
}

const MONIKER_INTERPOLATION: &str = "{MONIKER}";
const INSTANCE_ID_INTERPOLATION: &str = "{INSTANCE_ID}";

/// Returns Ok(None) if the template needs a component ID and there's none available for
/// the component. This is not an error and should be handled silently.
fn interpolate_template(
    template: &str,
    component_info: &ComponentIdInfo,
) -> Result<Option<String>, anyhow::Error> {
    let moniker_position = template.find(MONIKER_INTERPOLATION);
    let instance_id_position = template.find(INSTANCE_ID_INTERPOLATION);
    let separator_position = template.find(":");
    // If the insert position is before the first colon, it's the selector's moniker and
    // slashes should not be escaped.
    // Otherwise, treat the moniker string as a single Node or Property name,
    // and escape the appropriate characters.
    // Instance IDs have no special characters and don't need escaping.
    match (moniker_position, separator_position, instance_id_position, &component_info.instance_id)
    {
        (Some(i), Some(s), _, _) if i < s => {
            Ok(Some(template.replace(MONIKER_INTERPOLATION, &component_info.moniker.to_string())))
        }
        (Some(_), Some(_), _, _) => Ok(Some(template.replace(
            MONIKER_INTERPOLATION,
            &selectors::sanitize_string_for_selectors(&component_info.moniker.to_string()),
        ))),
        (_, _, Some(_), Some(id)) => {
            Ok(Some(template.replace(INSTANCE_ID_INTERPOLATION, &id.to_string())))
        }
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

pub fn moniker_deserialize<'de, D>(deserializer: D) -> Result<ExtendedMoniker, D::Error>
where
    D: Deserializer<'de>,
{
    let moniker_str = String::deserialize(deserializer)?;
    ExtendedMoniker::parse_str(&moniker_str).map_err(de::Error::custom)
}

pub fn instance_id_deserialize<'de, D>(deserializer: D) -> Result<Option<InstanceId>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        None => Ok(None),
        Some(instance_id) => {
            let instance_id = InstanceId::from_str(&instance_id).map_err(de::Error::custom)?;
            Ok(Some(instance_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moniker::Moniker;
    use std::str::FromStr;

    #[fuchsia::test]
    fn parse_project_template() {
        let template = r#"{
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
        }"#;
        let config: ProjectTemplate = serde_json5::from_str(template).expect("deserialize");
        assert_eq!(
            config,
            ProjectTemplate {
                project_id: ProjectId(13),
                poll_rate_sec: 60,
                customer_id: CustomerId(8),
                metrics: vec![MetricTemplate {
                    selectors: vec![
                        "{MONIKER}:root/path2:leaf2".into(),
                        "foo/bar:root/{MONIKER}:leaf3".into(),
                        "asdf/qwer:root/path4:pre-{MONIKER}-post".into(),
                    ],
                    event_codes: vec![],
                    project_id: None,
                    upload_once: false,
                    metric_id: MetricId(2),
                    metric_type: MetricType::Occurrence,
                }],
            }
        );
    }

    #[fuchsia::test]
    fn parse_component_config() {
        let components_json = r#"[
            {
                "id": 42,
                "label": "Foo_42",
                "moniker": "core/foo42"
            },
            {
                "id": 10,
                "label": "Hello",
                "instance_id": "8775ff0afe12ca578135014a5d36a7733b0f9982bcb62a888b007cb2c31a7046",
                "moniker": "bootstrap/hello"
            }
        ]"#;
        let config: ComponentIdInfoList =
            serde_json5::from_str(components_json).expect("deserialize");
        assert_eq!(
            config,
            ComponentIdInfoList(vec![
                ComponentIdInfo {
                    moniker: ExtendedMoniker::parse_str("core/foo42").unwrap(),
                    instance_id: None,
                    event_id: EventCode(42),
                    label: "Foo_42".into()
                },
                ComponentIdInfo {
                    moniker: ExtendedMoniker::parse_str("bootstrap/hello").unwrap(),
                    instance_id: Some(
                        InstanceId::from_str(
                            "8775ff0afe12ca578135014a5d36a7733b0f9982bcb62a888b007cb2c31a7046"
                        )
                        .unwrap()
                    ),
                    event_id: EventCode(10),
                    label: "Hello".into()
                },
            ])
        );
    }

    #[fuchsia::test]
    fn template_expansion_basic() {
        let project_template = ProjectTemplate {
            project_id: ProjectId(13),
            customer_id: CustomerId(7),
            poll_rate_sec: 60,
            metrics: vec![MetricTemplate {
                selectors: vec!["{MONIKER}:root/path:leaf".into()],
                metric_id: MetricId(1),
                metric_type: MetricType::Occurrence,
                event_codes: vec![EventCode(1), EventCode(2)],
                project_id: None,
                upload_once: true,
            }],
        };
        let component_info = ComponentIdInfoList(vec![
            ComponentIdInfo {
                moniker: ExtendedMoniker::parse_str("core/foo42").unwrap(),
                instance_id: None,
                event_id: EventCode(42),
                label: "Foo_42".into(),
            },
            ComponentIdInfo {
                moniker: ExtendedMoniker::parse_str("bootstrap/hello").unwrap(),
                instance_id: Some(
                    InstanceId::from_str(
                        "8775ff0afe12ca578135014a5d36a7733b0f9982bcb62a888b007cb2c31a7046",
                    )
                    .unwrap(),
                ),
                event_id: EventCode(43),
                label: "Hello".into(),
            },
        ]);
        let config = project_template.expand(&component_info);
        assert_eq!(
            config,
            ProjectConfig {
                project_id: ProjectId(13),
                customer_id: CustomerId(7),
                poll_rate_sec: 60,
                metrics: vec![
                    MetricConfig {
                        selectors: vec!["core/foo42:root/path:leaf".into(),],
                        metric_id: MetricId(1),
                        metric_type: MetricType::Occurrence,
                        event_codes: vec![EventCode(42), EventCode(1), EventCode(2)],
                        upload_once: true,
                        project_id: None,
                    },
                    MetricConfig {
                        selectors: vec!["bootstrap/hello:root/path:leaf".into(),],
                        metric_id: MetricId(1),
                        metric_type: MetricType::Occurrence,
                        event_codes: vec![EventCode(43), EventCode(1), EventCode(2)],
                        upload_once: true,
                        project_id: None,
                    },
                ],
            }
        );
    }

    #[fuchsia::test]
    fn template_expansion_many_selectors() {
        let project_template = ProjectTemplate {
            project_id: ProjectId(13),
            poll_rate_sec: 60,
            customer_id: CustomerId(7),
            metrics: vec![MetricTemplate {
                selectors: vec![
                    "{MONIKER}:root/path2:leaf2".into(),
                    "foo/bar:root/{MONIKER}:leaf3".into(),
                    "asdf/qwer:root/path4:pre-{MONIKER}-post".into(),
                ],
                metric_id: MetricId(2),
                metric_type: MetricType::Occurrence,
                event_codes: vec![],
                project_id: None,
                upload_once: false,
            }],
        };
        let component_info = ComponentIdInfoList(vec![
            ComponentIdInfo {
                moniker: ExtendedMoniker::parse_str("core/foo42").unwrap(),
                instance_id: None,
                event_id: EventCode(42),
                label: "Foo_42".into(),
            },
            ComponentIdInfo {
                moniker: ExtendedMoniker::parse_str("bootstrap/hello").unwrap(),
                instance_id: Some(
                    InstanceId::from_str(
                        "8775ff0afe12ca578135014a5d36a7733b0f9982bcb62a888b007cb2c31a7046",
                    )
                    .unwrap(),
                ),
                event_id: EventCode(43),
                label: "Hello".into(),
            },
        ]);
        let config = project_template.expand(&component_info);
        assert_eq!(
            config,
            ProjectConfig {
                project_id: ProjectId(13),
                customer_id: CustomerId(7),
                poll_rate_sec: 60,
                metrics: vec![
                    MetricConfig {
                        selectors: vec![
                            "core/foo42:root/path2:leaf2".into(),
                            "foo/bar:root/core\\/foo42:leaf3".into(),
                            "asdf/qwer:root/path4:pre-core\\/foo42-post".into()
                        ],
                        metric_id: MetricId(2),
                        metric_type: MetricType::Occurrence,
                        event_codes: vec![EventCode(42)],
                        upload_once: false,
                        project_id: None,
                    },
                    MetricConfig {
                        selectors: vec![
                            "bootstrap/hello:root/path2:leaf2".into(),
                            "foo/bar:root/bootstrap\\/hello:leaf3".into(),
                            "asdf/qwer:root/path4:pre-bootstrap\\/hello-post".into()
                        ],
                        metric_id: MetricId(2),
                        metric_type: MetricType::Occurrence,
                        event_codes: vec![EventCode(43)],
                        upload_once: false,
                        project_id: None,
                    },
                ],
            }
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
                event_id: EventCode(101),
                label: "bq".into(),
                instance_id: None,
            },
            ComponentIdInfo {
                moniker: "foo/bar".try_into().unwrap(),
                event_id: EventCode(102),
                label: "fb".into(),
                instance_id: None,
            },
        ];
        add_instance_ids(ids, &mut components);
        let moniker_template = "fizz/buzz:root/info/{MONIKER}/data";
        let id_template = "fizz/buzz:root/info/{INSTANCE_ID}/data";
        assert_eq!(
            interpolate_template(moniker_template, &components[0]).unwrap().unwrap(),
            "fizz/buzz:root/info/baz\\/quux/data".to_string()
        );
        assert_eq!(
            interpolate_template(moniker_template, &components[1]).unwrap().unwrap(),
            "fizz/buzz:root/info/foo\\/bar/data".to_string()
        );
        assert_eq!(interpolate_template(id_template, &components[0]).unwrap(), None);
        assert_eq!(
                interpolate_template(id_template, &components[1]).unwrap().unwrap(),
                "fizz/buzz:root/info/1234123412341234123412341234123412341234123412341234123412341234/data"
            );
    }

    fn add_instance_ids(
        ids: component_id_index::Index,
        fire_components: &mut Vec<ComponentIdInfo>,
    ) {
        for component in fire_components {
            if let ExtendedMoniker::ComponentInstance(moniker) = &component.moniker {
                component.instance_id = ids.id_for_moniker(moniker).cloned();
            }
        }
    }
}
