// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use itertools::Itertools as _;

/// The location where [`inspect_for_sampler_test_inner`] will pull Sampler
/// config from. All tests must make sure to output sampler config into that
/// directory (minus the leading "/pkg") in their dependencies.
const SAMPLER_CONFIG_DIR: &str = "/pkg/data/sampler-config";

/// Encapsulate logic for getting inspect data between NS2 and NS3, which sadly
/// have slightly different methods of doing so.
pub(crate) trait InspectDataGetter {
    async fn get_inspect_data(&self, metric: &str) -> diagnostics_hierarchy::DiagnosticsHierarchy;
}

/// The core part of the NS2 and NS3 test that validates that the selectors used
/// in the Sampler config are present in the inspect data.
pub(crate) async fn inspect_for_sampler_test_inner<S: InspectDataGetter>(getter: &S) {
    let sampler_config = sampler_config::SamplerConfigBuilder::default()
        .sampler_dir(SAMPLER_CONFIG_DIR)
        .load()
        .expect("SamplerConfig load failed");
    let project_config = match &sampler_config.project_configs[..] {
        [project_config] => project_config,
        project_configs => panic!("expected one project_config but got {:#?}", project_configs),
    };
    for metric_config in &project_config.metrics {
        let selector = match &metric_config.selectors[..] {
            [selector] => &selector.selector,
            selectors => panic!("expected one selector but got {:#?}", selectors),
        };
        let fidl_fuchsia_diagnostics::Selector { tree_selector, .. } = selector;
        let (tree_selector, expected_key) = match tree_selector.as_ref().expect("tree_selector") {
            fidl_fuchsia_diagnostics::TreeSelector::PropertySelector(
                fidl_fuchsia_diagnostics::PropertySelector { node_path, target_properties },
            ) => {
                let tree_selector = node_path
                    .iter()
                    .map(|selector| match selector {
                        fidl_fuchsia_diagnostics::StringSelector::ExactMatch(segment) => {
                            selectors::sanitize_string_for_selectors(segment)
                        }
                        selector => panic!("expected exact match selector but got {:#?}", selector),
                    })
                    .join("/");
                let expected_key = match target_properties {
                    fidl_fuchsia_diagnostics::StringSelector::ExactMatch(segment) => segment,
                    selector => panic!("expected exact match selector but got {:#?}", selector),
                };
                (tree_selector, expected_key)
            }
            selector => panic!("expected property selector but got {:#?}", selector),
        };
        let data = getter.get_inspect_data(&format!("{tree_selector}:{expected_key}")).await;
        let properties: Vec<_> = data
            .property_iter()
            .filter_map(|(_hierarchy_path, property_opt): (Vec<&str>, _)| property_opt)
            .collect();
        match &properties[..] {
            [diagnostics_hierarchy::Property::Uint(key, _)] => {
                if key != expected_key {
                    panic!(
                        "wrong key {:#?} found (expected {:#?}) for selector {:#?}",
                        key, expected_key, selector
                    );
                }
            }
            [] => {
                panic!("no properties found for selector {:#?}", selector)
            }
            properties => {
                panic!("wrong properties {:#?} found for selector {:#?}", properties, selector);
            }
        }
    }
}
