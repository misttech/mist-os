// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::{CustomerId, EventCode, MetricId, MetricType, ProjectId};
use component_id_index::InstanceId;
use moniker::ExtendedMoniker;
use serde::Deserialize;

// At the moment there's no difference between the user facing project config and the one loaded at
// runtime. This will change as we integrate Cobalt mappings.
pub use crate::runtime::{MetricConfig, ProjectConfig};

/// Configuration for a single FIRE project template to map Inspect data to its Cobalt metrics
/// for all components in the ComponentIdInfo. Just like ProjectConfig except it uses MetricTemplate
/// instead of MetricConfig.
#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct FireProjectTemplate {
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

/// Configuration for a single FIRE metric template to map from an Inspect property
/// to a cobalt metric. Unlike MetricConfig, selectors aren't parsed.
#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
pub struct MetricTemplate {
    /// Selector identifying the metric to
    /// sample via the diagnostics platform.
    #[serde(rename = "selector", deserialize_with = "crate::utils::one_or_many_strings")]
    selectors: Vec<String>,

    /// Cobalt metric id to map the selector to.
    metric_id: MetricId,

    /// Data type to transform the metric to.
    metric_type: MetricType,

    /// Event codes defining the dimensions of the
    /// cobalt metric.
    /// Notes:
    /// - Order matters, and must match the order of the defined dimensions
    ///   in the cobalt metric file.
    /// - The FIRE component-ID will be inserted as the first element of event_codes.
    /// - The event_codes field may be omitted from the config file if component-ID is the only
    ///   event code.
    event_codes: Option<Vec<EventCode>>,

    /// Optional boolean specifying whether to upload the specified metric only once, the first time
    /// it becomes available to the sampler. Defaults to false.
    #[serde(default)]
    upload_once: bool,

    /// Optional project id. When present this project id will be used instead of the top-level
    /// project id.
    // TODO(https://fxbug.dev/42071858): remove this when we support batching.
    project_id: Option<u32>,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ComponentIdInfoList(Vec<ComponentIdInfo>);

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub struct ComponentIdInfo {
    /// The component's moniker
    #[serde(deserialize_with = "crate::utils::moniker_deserialize")]
    pub moniker: ExtendedMoniker,

    /// The Component Instance ID - may not be available
    #[serde(default, deserialize_with = "crate::utils::instance_id_deserialize")]
    pub instance_id: Option<InstanceId>,

    /// The ID sent to Cobalt as an event code
    #[serde(alias = "id")]
    pub event_id: EventCode,

    /// Human-readable label, not used by Sampler.
    #[allow(unused)]
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let config: FireProjectTemplate = serde_json5::from_str(template).expect("deserialize");
        assert_eq!(
            config,
            FireProjectTemplate {
                project_id: ProjectId(13),
                poll_rate_sec: 60,
                customer_id: CustomerId(8),
                metrics: vec![MetricTemplate {
                    selectors: vec![
                        "{MONIKER}:root/path2:leaf2".into(),
                        "foo/bar:root/{MONIKER}:leaf3".into(),
                        "asdf/qwer:root/path4:pre-{MONIKER}-post".into(),
                    ],
                    event_codes: None,
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
}
