// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_driver_framework as fdf;

#[cfg(test)]
pub fn make_accept(key: &str, value: fdf::NodePropertyValue) -> fdf::BindRule {
    fdf::BindRule {
        key: fdf::NodePropertyKey::StringValue(key.to_string()),
        condition: fdf::Condition::Accept,
        values: vec![value],
    }
}

#[cfg(test)]
pub fn make_accept_list(key: &str, values: Vec<fdf::NodePropertyValue>) -> fdf::BindRule {
    fdf::BindRule {
        key: fdf::NodePropertyKey::StringValue(key.to_string()),
        condition: fdf::Condition::Accept,
        values: values,
    }
}

#[cfg(test)]
pub fn make_parent_spec(
    bind_rules: Vec<fdf::BindRule>,
    properties: Vec<fdf::NodeProperty>,
) -> fdf::ParentSpec {
    fdf::ParentSpec { bind_rules, properties }
}

#[cfg(test)]
pub fn make_composite_spec(
    spec_name: &str,
    parents: Vec<fdf::ParentSpec>,
) -> fdf::CompositeNodeSpec {
    fdf::CompositeNodeSpec {
        name: Some(spec_name.to_string()),
        parents: Some(parents),
        ..Default::default()
    }
}

#[cfg(test)]
pub fn make_property(key: &str, value: fdf::NodePropertyValue) -> fdf::NodeProperty {
    fdf::NodeProperty { key: fdf::NodePropertyKey::StringValue(key.to_string()), value: value }
}
