// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::match_common::{get_composite_rules_from_composite_driver, node_to_device_property};
use crate::resolved_driver::ResolvedDriver;
use crate::serde_ext::{CompositeInfoDef, ConditionDef};
use bind::compiler::symbol_table::{get_deprecated_key_identifier, get_deprecated_key_value};
use bind::compiler::Symbol;
use bind::interpreter::decode_bind_rules::DecodedRules;
use bind::interpreter::match_bind::{match_bind, DeviceProperties, MatchBindData, PropertyKey};
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, HashMap, HashSet};
use zx::sys::zx_status_t;
use zx::Status;
use {fidl_fuchsia_driver_framework as fdf, fidl_fuchsia_driver_index as fdi};

const NAME_REGEX: &'static str = r"^[a-zA-Z0-9\-_]*$";

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct BindRuleCondition {
    #[serde(with = "ConditionDef")]
    condition: fdf::Condition,
    values: Vec<Symbol>,
}

type BindRules = BTreeMap<PropertyKey, BindRuleCondition>;

#[derive(Clone, Serialize, Deserialize)]
pub struct CompositeParentRef {
    // This is the spec name. Corresponds with the key of spec_list.
    pub name: String,

    // This is the parent index.
    pub index: u32,
}

pub fn serialize_parent_refs<S>(
    target: &HashMap<BindRules, Vec<CompositeParentRef>>,
    ser: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let container = target
        .into_iter()
        .map(|entry| (entry.0.into_iter().collect::<Vec<_>>(), entry.1))
        .collect::<Vec<_>>();
    serde::Serialize::serialize(&container, ser)
}

pub fn deserialize_parent_refs<'de, D>(
    des: D,
) -> Result<HashMap<BindRules, Vec<CompositeParentRef>>, D::Error>
where
    D: Deserializer<'de>,
{
    let container: Vec<(Vec<(PropertyKey, BindRuleCondition)>, Vec<CompositeParentRef>)> =
        serde::Deserialize::deserialize(des)?;

    let result: HashMap<BindRules, Vec<CompositeParentRef>> = HashMap::from_iter(
        container.into_iter().map(|entry| (BindRules::from_iter(entry.0.into_iter()), entry.1)),
    );
    Ok(result)
}

pub fn serialize_spec_list<S>(
    spec_list: &HashMap<String, fdf::CompositeInfo>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    #[derive(Serialize)]
    struct Wrapper<'a>(#[serde(with = "CompositeInfoDef")] &'a fdf::CompositeInfo);

    let converted = spec_list.iter().map(|(_name, sl)| Wrapper(sl)).collect::<Vec<_>>();
    serde::Serialize::serialize(&converted, serializer)
}

fn deserialize_spec_list<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, fdf::CompositeInfo>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Wrapper(#[serde(with = "CompositeInfoDef")] fdf::CompositeInfo);

    let wrapper_vec: Vec<Wrapper> = serde::Deserialize::deserialize(deserializer)?;
    Ok(HashMap::from_iter(wrapper_vec.into_iter().map(|entry| {
        let name = entry.0.spec.as_ref().and_then(|s| s.name.as_ref()).unwrap().to_owned();
        (name, entry.0)
    })))
}

// The CompositeNodeSpecManager struct is responsible of managing a list of specs
// for matching.
#[derive(Serialize, Deserialize, Default)]

pub struct CompositeNodeSpecManager {
    // Maps a list of specs to the bind rules of their nodes. This is to handle multiple
    // specs that share a node with the same bind rules. Used for matching nodes.
    //
    // This requires the custom serializer/deserializer as serde does not handle maps
    // with non-string keys. Both the root HashMap and the inner BTree (BindRules type)
    // get converted to vectors in the custom serialization and from vectors in the deserialization.
    #[serde(
        serialize_with = "serialize_parent_refs",
        deserialize_with = "deserialize_parent_refs"
    )]
    pub parent_refs: HashMap<BindRules, Vec<CompositeParentRef>>,

    // Maps specs to the name. This list ensures that we don't add multiple specs with
    // the same name.
    //
    // This requires a custom serializer/deserializer as the value type is a fidl type without
    // built-in serde support. We manually create a local duplicate of the type using serde's remote
    // types feature and use a wrapper type to do the serialization/deserialization for us.
    #[serde(serialize_with = "serialize_spec_list", deserialize_with = "deserialize_spec_list")]
    pub spec_list: HashMap<String, fdf::CompositeInfo>,
}

impl CompositeNodeSpecManager {
    pub fn new() -> Self {
        CompositeNodeSpecManager { parent_refs: HashMap::new(), spec_list: HashMap::new() }
    }

    pub fn add_composite_node_spec(
        &mut self,
        spec: fdf::CompositeNodeSpec,
        composite_drivers: Vec<&ResolvedDriver>,
    ) -> Result<(), i32> {
        // Get and validate the name.
        let name = spec.name.clone().ok_or_else(|| Status::INVALID_ARGS.into_raw())?;
        if let Ok(name_regex) = Regex::new(NAME_REGEX) {
            if !name_regex.is_match(&name) {
                tracing::error!(
                    "Invalid spec name. Name can only contain [A-Za-z0-9-_] characters"
                );
                return Err(Status::INVALID_ARGS.into_raw());
            }
        } else {
            tracing::warn!("Regex failure. Unable to validate spec name");
        }

        let parents = spec.parents.clone().ok_or_else(|| Status::INVALID_ARGS.into_raw())?;

        if self.spec_list.contains_key(&name) {
            return Err(Status::ALREADY_EXISTS.into_raw());
        }

        if parents.is_empty() {
            return Err(Status::INVALID_ARGS.into_raw());
        }

        // Collect parent refs in a separate vector before adding them to the
        // CompositeNodeSpecManager. This is to ensure that we add the parent refs after
        // they're all verified to be valid.
        // TODO(https://fxbug.dev/42056805): Update tests so that we can verify that properties exists in
        // each parent ref.
        let mut parent_refs: Vec<(BindRules, CompositeParentRef)> = vec![];
        for (idx, parent) in parents.iter().enumerate() {
            let properties = convert_fidl_to_bind_rules(&parent.bind_rules)?;
            parent_refs
                .push((properties, CompositeParentRef { name: name.clone(), index: idx as u32 }));
        }

        // Add each parent ref into the map.
        for (properties, parent_ref) in parent_refs {
            self.parent_refs
                .entry(properties)
                .and_modify(|refs| refs.push(parent_ref.clone()))
                .or_insert_with(|| vec![parent_ref]);
        }

        let matched_composite_result =
            self.find_composite_driver_match(&parents, &composite_drivers);

        if let Some(matched_composite) = &matched_composite_result {
            tracing::info!(
                "Matched '{}' to composite node spec '{}'",
                get_driver_url(matched_composite),
                name
            );
        }

        self.spec_list.insert(
            name,
            fdf::CompositeInfo {
                spec: Some(spec),
                matched_driver: matched_composite_result,
                ..Default::default()
            },
        );
        Ok(())
    }

    // Match the given device properties to all the nodes. Returns a list of specs for all the
    // nodes that match.
    pub fn match_parent_specs(
        &self,
        properties: &DeviceProperties,
    ) -> Option<fdi::MatchDriverResult> {
        let mut matching_refs: Vec<CompositeParentRef> = vec![];
        for (node_props, parent_ref_list) in self.parent_refs.iter() {
            if match_node(&node_props, properties) {
                matching_refs.extend_from_slice(parent_ref_list.as_slice());
            }
        }

        if matching_refs.is_empty() {
            return None;
        }

        // Put in the matched composite info for this spec that we have stored in
        // |spec_list|.
        let mut composite_parents_result: Vec<fdf::CompositeParent> = vec![];
        for matching_ref in matching_refs {
            let composite_info = self.spec_list.get(&matching_ref.name);
            match composite_info {
                Some(info) => {
                    // TODO(https://fxbug.dev/42058749): Only return specs that have a matched composite using
                    // info.matched_driver.is_some()
                    composite_parents_result.push(fdf::CompositeParent {
                        composite: Some(fdf::CompositeInfo {
                            spec: Some(strip_parents_from_spec(&info.spec)),
                            matched_driver: info.matched_driver.clone(),
                            ..Default::default()
                        }),
                        index: Some(matching_ref.index),
                        ..Default::default()
                    });
                }
                None => {}
            }
        }

        if composite_parents_result.is_empty() {
            return None;
        }

        Some(fdi::MatchDriverResult::CompositeParents(composite_parents_result))
    }

    pub fn new_driver_available(&mut self, resolved_driver: &ResolvedDriver) {
        // Only composite drivers should be matched against composite node specs.
        if matches!(resolved_driver.bind_rules, DecodedRules::Normal(_)) {
            return;
        }

        for (name, composite_info) in self.spec_list.iter_mut() {
            if composite_info.matched_driver.is_some() {
                continue;
            }

            let parents = composite_info.spec.as_ref().unwrap().parents.as_ref().unwrap();
            let matched_composite_result = match_composite_properties(resolved_driver, parents);
            if let Ok(Some(matched_composite)) = matched_composite_result {
                tracing::info!(
                    "Matched '{}' to composite node spec '{}'",
                    get_driver_url(&matched_composite),
                    name
                );
                composite_info.matched_driver = Some(matched_composite);
            }
        }
    }

    pub fn rebind(
        &mut self,
        spec_name: String,
        composite_drivers: Vec<&ResolvedDriver>,
    ) -> Result<(), zx_status_t> {
        let composite_info =
            self.spec_list.get(&spec_name).ok_or_else(|| Status::NOT_FOUND.into_raw())?;
        let parents = composite_info
            .spec
            .as_ref()
            .ok_or_else(|| Status::INTERNAL.into_raw())?
            .parents
            .as_ref()
            .ok_or_else(|| Status::INTERNAL.into_raw())?;
        let new_match = self.find_composite_driver_match(parents, &composite_drivers);
        self.spec_list.entry(spec_name).and_modify(|spec| {
            spec.matched_driver = new_match;
        });
        Ok(())
    }

    pub fn rebind_composites_with_driver(
        &mut self,
        driver: String,
        composite_drivers: Vec<&ResolvedDriver>,
    ) -> Result<(), zx_status_t> {
        let specs_to_rebind = self
            .spec_list
            .iter()
            .filter_map(|(spec_name, spec_info)| {
                spec_info
                    .matched_driver
                    .as_ref()
                    .and_then(|matched_driver| matched_driver.composite_driver.as_ref())
                    .and_then(|composite_driver| composite_driver.driver_info.as_ref())
                    .and_then(|driver_info| driver_info.url.as_ref())
                    .and_then(|url| if &driver == url { Some(spec_name.to_string()) } else { None })
            })
            .collect::<Vec<_>>();

        for spec_name in specs_to_rebind {
            let composite_info =
                self.spec_list.get(&spec_name).ok_or_else(|| Status::NOT_FOUND.into_raw())?;
            let parents = composite_info
                .spec
                .as_ref()
                .ok_or_else(|| Status::INTERNAL.into_raw())?
                .parents
                .as_ref()
                .ok_or_else(|| Status::INTERNAL.into_raw())?;
            let new_match = self.find_composite_driver_match(parents, &composite_drivers);
            self.spec_list.entry(spec_name).and_modify(|spec| {
                spec.matched_driver = new_match;
            });
        }

        Ok(())
    }

    pub fn get_specs(&self, name_filter: Option<String>) -> Vec<fdf::CompositeInfo> {
        if let Some(name) = name_filter {
            match self.spec_list.get(&name) {
                Some(item) => return vec![item.clone()],
                None => return vec![],
            }
        };

        let specs = self
            .spec_list
            .iter()
            .map(|(_name, composite_info)| composite_info.clone())
            .collect::<Vec<_>>();

        return specs;
    }

    fn find_composite_driver_match<'a>(
        &self,
        parents: &'a Vec<fdf::ParentSpec>,
        composite_drivers: &Vec<&ResolvedDriver>,
    ) -> Option<fdf::CompositeDriverMatch> {
        for composite_driver in composite_drivers {
            let matched_composite = match_composite_properties(composite_driver, parents);
            if let Ok(Some(matched_composite)) = matched_composite {
                return Some(matched_composite);
            }
        }
        None
    }
}

pub fn strip_parents_from_spec(spec: &Option<fdf::CompositeNodeSpec>) -> fdf::CompositeNodeSpec {
    // Strip the parents of the rules and properties since they are not needed by
    // the driver manager.
    let parents_stripped = spec.as_ref().and_then(|spec| spec.parents.as_ref()).map(|parents| {
        parents
            .iter()
            .map(|_parent| fdf::ParentSpec { bind_rules: vec![], properties: vec![] })
            .collect::<Vec<_>>()
    });

    fdf::CompositeNodeSpec {
        name: spec.as_ref().and_then(|spec| spec.name.clone()),
        parents: parents_stripped,
        ..Default::default()
    }
}

fn convert_fidl_to_bind_rules(
    fidl_bind_rules: &Vec<fdf::BindRule>,
) -> Result<BindRules, zx_status_t> {
    if fidl_bind_rules.is_empty() {
        return Err(Status::INVALID_ARGS.into_raw());
    }

    let mut bind_rules = BTreeMap::new();
    for fidl_rule in fidl_bind_rules {
        let key = match &fidl_rule.key {
            fdf::NodePropertyKey::IntValue(i) => PropertyKey::NumberKey(i.clone().into()),
            fdf::NodePropertyKey::StringValue(s) => PropertyKey::StringKey(s.clone()),
        };

        // Check if the properties contain duplicate keys.
        if bind_rules.contains_key(&key) {
            return Err(Status::INVALID_ARGS.into_raw());
        }

        let first_val = fidl_rule.values.first().ok_or_else(|| Status::INVALID_ARGS.into_raw())?;
        let values = fidl_rule
            .values
            .iter()
            .map(|val| {
                // Check that the properties are all the same type.
                if std::mem::discriminant(first_val) != std::mem::discriminant(val) {
                    return Err(Status::INVALID_ARGS.into_raw());
                }
                Ok(node_property_to_symbol(val)?)
            })
            .collect::<Result<Vec<Symbol>, zx_status_t>>()?;

        bind_rules
            .insert(key, BindRuleCondition { condition: fidl_rule.condition, values: values });
    }
    Ok(bind_rules)
}

fn match_node(bind_rules: &BindRules, device_properties: &DeviceProperties) -> bool {
    for (key, node_prop_values) in bind_rules.iter() {
        let mut dev_prop_contains_value = match device_properties.get(key) {
            Some(val) => node_prop_values.values.contains(val),
            None => false,
        };

        // If the properties don't contain the key, try to convert it to a deprecated
        // key and check the properties with it.
        if !dev_prop_contains_value && !device_properties.contains_key(key) {
            let deprecated_key = match key {
                PropertyKey::NumberKey(int_key) => get_deprecated_key_identifier(*int_key as u32)
                    .map(|key| PropertyKey::StringKey(key)),
                PropertyKey::StringKey(str_key) => {
                    get_deprecated_key_value(str_key).map(|key| PropertyKey::NumberKey(key as u64))
                }
            };

            if let Some(key) = deprecated_key {
                dev_prop_contains_value = match device_properties.get(&key) {
                    Some(val) => node_prop_values.values.contains(val),
                    None => false,
                };
            }
        }

        let evaluate_condition = match node_prop_values.condition {
            fdf::Condition::Accept => {
                // If the node property accepts a false boolean value and the property is
                // missing from the device properties, then we should evaluate the condition
                // as true.
                dev_prop_contains_value
                    || node_prop_values.values.contains(&Symbol::BoolValue(false))
            }
            fdf::Condition::Reject => !dev_prop_contains_value,
            fdf::Condition::Unknown => {
                tracing::error!("Invalid condition type in bind rules.");
                return false;
            }
        };

        if !evaluate_condition {
            return false;
        }
    }

    true
}

fn node_property_to_symbol(value: &fdf::NodePropertyValue) -> Result<Symbol, zx_status_t> {
    match value {
        fdf::NodePropertyValue::IntValue(i) => {
            Ok(bind::compiler::Symbol::NumberValue(i.clone().into()))
        }
        fdf::NodePropertyValue::StringValue(s) => {
            Ok(bind::compiler::Symbol::StringValue(s.clone()))
        }
        fdf::NodePropertyValue::EnumValue(s) => Ok(bind::compiler::Symbol::EnumValue(s.clone())),
        fdf::NodePropertyValue::BoolValue(b) => Ok(bind::compiler::Symbol::BoolValue(b.clone())),
        _ => Err(Status::INVALID_ARGS.into_raw()),
    }
}

fn match_composite_properties<'a>(
    composite_driver: &'a ResolvedDriver,
    parents: &'a Vec<fdf::ParentSpec>,
) -> Result<Option<fdf::CompositeDriverMatch>, i32> {
    // The spec must have at least 1 node to match a composite driver.
    if parents.len() < 1 {
        return Ok(None);
    }

    let composite = get_composite_rules_from_composite_driver(composite_driver)?;

    // The composite driver bind rules should have a total node count of more than or equal to the
    // total node count of the spec. This is to account for optional nodes in the
    // composite driver bind rules.
    if composite.optional_nodes.len() + composite.additional_nodes.len() + 1 < parents.len() {
        return Ok(None);
    }

    // First find a matching primary node.
    let mut primary_parent_index = 0;
    let mut primary_matches = false;
    for i in 0..parents.len() {
        primary_matches = node_matches_composite_driver(
            &parents[i],
            &composite.primary_node.instructions,
            &composite.symbol_table,
        );
        if primary_matches {
            primary_parent_index = i as u32;
            break;
        }
    }

    if !primary_matches {
        return Ok(None);
    }

    // The remaining nodes in the properties can match the
    // additional nodes in the bind rules in any order.
    //
    // This logic has one issue that we are accepting as a tradeoff for simplicity:
    // If a properties node can match to multiple bind rule
    // additional nodes, it is going to take the first one, even if there is a less strict
    // node that it can take. This can lead to false negative matches.
    //
    // Example:
    // properties[1] can match both additional_nodes[0] and additional_nodes[1]
    // properties[2] can only match additional_nodes[0]
    //
    // This algorithm will return false because it matches up properties[1] with
    // additional_nodes[0], and so properties[2] can't match the remaining nodes
    // [additional_nodes[1]].
    //
    // If we were smarter here we could match up properties[1] with additional_nodes[1]
    // and properties[2] with additional_nodes[0] to return a positive match.
    // TODO(https://fxbug.dev/42058532): Disallow ambiguity with spec matching. We should log
    // a warning and return false if a spec node matches with multiple composite
    // driver nodes, and vice versa.
    let mut unmatched_additional_indices =
        (0..composite.additional_nodes.len()).collect::<HashSet<_>>();
    let mut unmatched_optional_indices =
        (0..composite.optional_nodes.len()).collect::<HashSet<_>>();

    let mut parent_names = vec![];

    for i in 0..parents.len() {
        if i == primary_parent_index as usize {
            parent_names.push(composite.symbol_table[&composite.primary_node.name_id].clone());
            continue;
        }

        let mut matched = None;
        let mut matched_name: Option<String> = None;
        let mut from_optional = false;

        // First check if any of the additional nodes match it.
        for &j in &unmatched_additional_indices {
            let matches = node_matches_composite_driver(
                &parents[i],
                &composite.additional_nodes[j].instructions,
                &composite.symbol_table,
            );
            if matches {
                matched = Some(j);
                matched_name =
                    Some(composite.symbol_table[&composite.additional_nodes[j].name_id].clone());
                break;
            }
        }

        // If no additional nodes matched it, then look in the optional nodes.
        if matched.is_none() {
            for &j in &unmatched_optional_indices {
                let matches = node_matches_composite_driver(
                    &parents[i],
                    &composite.optional_nodes[j].instructions,
                    &composite.symbol_table,
                );
                if matches {
                    from_optional = true;
                    matched = Some(j);
                    matched_name =
                        Some(composite.symbol_table[&composite.optional_nodes[j].name_id].clone());
                    break;
                }
            }
        }

        if matched.is_none() {
            return Ok(None);
        }

        if from_optional {
            unmatched_optional_indices.remove(&matched.unwrap());
        } else {
            unmatched_additional_indices.remove(&matched.unwrap());
        }

        parent_names.push(matched_name.unwrap());
    }

    // If we didn't consume all of the additional nodes in the bind rules then this is not a match.
    if !unmatched_additional_indices.is_empty() {
        return Ok(None);
    }

    let driver = fdf::CompositeDriverInfo {
        composite_name: Some(composite.symbol_table[&composite.device_name_id].clone()),
        driver_info: Some(composite_driver.create_driver_info(false)),
        ..Default::default()
    };
    return Ok(Some(fdf::CompositeDriverMatch {
        composite_driver: Some(driver),
        parent_names: Some(parent_names),
        primary_parent_index: Some(primary_parent_index),
        ..Default::default()
    }));
}

fn node_matches_composite_driver(
    node: &fdf::ParentSpec,
    bind_rules_node: &Vec<u8>,
    symbol_table: &HashMap<u32, String>,
) -> bool {
    match node_to_device_property(&node.properties) {
        Err(_) => false,
        Ok(props) => {
            let match_bind_data = MatchBindData { symbol_table, instructions: bind_rules_node };
            match_bind(match_bind_data, &props).unwrap_or(false)
        }
    }
}

fn get_driver_url(composite: &fdf::CompositeDriverMatch) -> String {
    return composite
        .composite_driver
        .as_ref()
        .and_then(|driver| driver.driver_info.as_ref())
        .and_then(|driver_info| driver_info.url.clone())
        .unwrap_or_else(|| "".to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolved_driver::DriverPackageType;
    use bind::compiler::{
        CompiledBindRules, CompositeBindRules, CompositeNode, SymbolicInstruction,
        SymbolicInstructionInfo,
    };
    use bind::parser::bind_library::ValueType;
    use fuchsia_async as fasync;

    const TEST_DEVICE_NAME: &str = "test_device";
    const TEST_PRIMARY_NAME: &str = "primary_node";
    const TEST_ADDITIONAL_A_NAME: &str = "node_a";
    const TEST_ADDITIONAL_B_NAME: &str = "node_b";
    const TEST_OPTIONAL_NAME: &str = "optional_node";

    fn make_accept(key: fdf::NodePropertyKey, value: fdf::NodePropertyValue) -> fdf::BindRule {
        fdf::BindRule { key: key, condition: fdf::Condition::Accept, values: vec![value] }
    }

    fn make_accept_list(
        key: fdf::NodePropertyKey,
        values: Vec<fdf::NodePropertyValue>,
    ) -> fdf::BindRule {
        fdf::BindRule { key: key, condition: fdf::Condition::Accept, values: values }
    }

    fn make_reject(key: fdf::NodePropertyKey, value: fdf::NodePropertyValue) -> fdf::BindRule {
        fdf::BindRule { key: key, condition: fdf::Condition::Reject, values: vec![value] }
    }

    fn make_reject_list(
        key: fdf::NodePropertyKey,
        values: Vec<fdf::NodePropertyValue>,
    ) -> fdf::BindRule {
        fdf::BindRule { key: key, condition: fdf::Condition::Reject, values: values }
    }

    fn make_property(
        key: fdf::NodePropertyKey,
        value: fdf::NodePropertyValue,
    ) -> fdf::NodeProperty {
        fdf::NodeProperty { key: key, value: value }
    }

    // TODO(https://fxbug.dev/42071377): Update tests so that they use the test data functions more often.
    fn create_test_parent_spec_1() -> fdf::ParentSpec {
        let bind_rules = vec![
            make_accept(fdf::NodePropertyKey::IntValue(1), fdf::NodePropertyValue::IntValue(200)),
            make_accept(fdf::NodePropertyKey::IntValue(3), fdf::NodePropertyValue::BoolValue(true)),
            make_accept(
                fdf::NodePropertyKey::StringValue("killdeer".to_string()),
                fdf::NodePropertyValue::StringValue("plover".to_string()),
            ),
        ];

        let properties = vec![make_property(
            fdf::NodePropertyKey::IntValue(2),
            fdf::NodePropertyValue::BoolValue(false),
        )];

        fdf::ParentSpec { bind_rules: bind_rules, properties: properties }
    }

    fn create_test_parent_spec_2() -> fdf::ParentSpec {
        let bind_rules = vec![
            make_reject(
                fdf::NodePropertyKey::StringValue("killdeer".to_string()),
                fdf::NodePropertyValue::StringValue("plover".to_string()),
            ),
            make_accept(
                fdf::NodePropertyKey::StringValue("flycatcher".to_string()),
                fdf::NodePropertyValue::EnumValue("flycatcher.phoebe".to_string()),
            ),
            make_reject(
                fdf::NodePropertyKey::StringValue("yellowlegs".to_string()),
                fdf::NodePropertyValue::BoolValue(true),
            ),
        ];

        let properties = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        fdf::ParentSpec { bind_rules: bind_rules, properties: properties }
    }

    fn create_driver<'a>(
        composite_name: String,
        primary_node: (&str, Vec<SymbolicInstructionInfo<'a>>),
        additionals: Vec<(&str, Vec<SymbolicInstructionInfo<'a>>)>,
        optionals: Vec<(&str, Vec<SymbolicInstructionInfo<'a>>)>,
    ) -> ResolvedDriver {
        let mut additional_nodes = vec![];
        let mut optional_nodes = vec![];
        for additional in additionals {
            additional_nodes
                .push(CompositeNode { name: additional.0.to_string(), instructions: additional.1 });
        }
        for optional in optionals {
            optional_nodes
                .push(CompositeNode { name: optional.0.to_string(), instructions: optional.1 });
        }
        let bind_rules = CompositeBindRules {
            device_name: composite_name,
            symbol_table: HashMap::new(),
            primary_node: CompositeNode {
                name: primary_node.0.to_string(),
                instructions: primary_node.1,
            },
            additional_nodes: additional_nodes,
            optional_nodes: optional_nodes,
            enable_debug: false,
        };

        let bytecode = CompiledBindRules::CompositeBind(bind_rules).encode_to_bytecode().unwrap();
        let rules = DecodedRules::new(bytecode).unwrap();

        ResolvedDriver {
            component_url: cm_types::Url::new(
                "fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm",
            )
            .unwrap(),
            bind_rules: rules,
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
            is_dfv2: None,
            disabled: false,
        }
    }

    fn create_driver_with_rules<'a>(
        primary_node: (&str, Vec<SymbolicInstructionInfo<'a>>),
        additionals: Vec<(&str, Vec<SymbolicInstructionInfo<'a>>)>,
        optionals: Vec<(&str, Vec<SymbolicInstructionInfo<'a>>)>,
    ) -> ResolvedDriver {
        create_driver(TEST_DEVICE_NAME.to_string(), primary_node, additionals, optionals)
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_property_match_node() {
        let nodes = Some(vec![create_test_parent_spec_1(), create_test_parent_spec_2()]);

        let composite_spec = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: nodes.clone(),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(composite_spec.clone(), vec![])
        );

        assert_eq!(1, composite_node_spec_manager.get_specs(None).len());
        assert_eq!(0, composite_node_spec_manager.get_specs(Some("not_there".to_string())).len());
        let specs = composite_node_spec_manager.get_specs(Some("test_spec".to_string()));
        assert_eq!(1, specs.len());
        let composite_node_spec = &specs[0];
        let expected_composite_node_spec =
            fdf::CompositeInfo { spec: Some(composite_spec.clone()), ..Default::default() };

        assert_eq!(&expected_composite_node_spec, composite_node_spec);

        // Match node 1.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(200));
        device_properties_1.insert(
            PropertyKey::StringKey("kingfisher".to_string()),
            Symbol::StringValue("kookaburra".to_string()),
        );
        device_properties_1.insert(PropertyKey::NumberKey(3), Symbol::BoolValue(true));
        device_properties_1.insert(
            PropertyKey::StringKey("killdeer".to_string()),
            Symbol::StringValue("plover".to_string()),
        );

        let expected_parent = fdf::CompositeParent {
            composite: Some(fdf::CompositeInfo {
                spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
                ..Default::default()
            }),
            index: Some(0),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent])),
            composite_node_spec_manager.match_parent_specs(&device_properties_1)
        );

        // Match node 2.
        let mut device_properties_2: DeviceProperties = HashMap::new();
        device_properties_2
            .insert(PropertyKey::StringKey("yellowlegs".to_string()), Symbol::BoolValue(false));
        device_properties_2.insert(
            PropertyKey::StringKey("killdeer".to_string()),
            Symbol::StringValue("lapwing".to_string()),
        );
        device_properties_2.insert(
            PropertyKey::StringKey("flycatcher".to_string()),
            Symbol::EnumValue("flycatcher.phoebe".to_string()),
        );

        let expected_parent_2 = fdf::CompositeParent {
            composite: Some(fdf::CompositeInfo {
                spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
                ..Default::default()
            }),
            index: Some(1),
            ..Default::default()
        };

        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent_2])),
            composite_node_spec_manager.match_parent_specs(&device_properties_2)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_property_match_bool_edgecase() {
        let bind_rules = vec![
            make_accept(fdf::NodePropertyKey::IntValue(1), fdf::NodePropertyValue::IntValue(200)),
            make_accept(
                fdf::NodePropertyKey::IntValue(3),
                fdf::NodePropertyValue::BoolValue(false),
            ),
        ];

        let properties = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let composite_spec = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: Some(vec![fdf::ParentSpec { bind_rules: bind_rules, properties: properties }]),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(composite_spec.clone(), vec![])
        );

        // Match node.
        let mut device_properties: DeviceProperties = HashMap::new();
        device_properties.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(200));

        let expected_parent = fdf::CompositeParent {
            composite: Some(fdf::CompositeInfo {
                spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
                ..Default::default()
            }),
            index: Some(0),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent])),
            composite_node_spec_manager.match_parent_specs(&device_properties)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_deprecated_keys_match() {
        let bind_rules = vec![
            make_accept(
                fdf::NodePropertyKey::StringValue("fuchsia.BIND_PROTOCOL".to_string()),
                fdf::NodePropertyValue::IntValue(200),
            ),
            make_accept(
                fdf::NodePropertyKey::IntValue(0x0201), // "fuchsia.BIND_USB_PID"
                fdf::NodePropertyValue::IntValue(10),
            ),
        ];

        let properties = vec![make_property(
            fdf::NodePropertyKey::IntValue(0x01),
            fdf::NodePropertyValue::IntValue(50),
        )];

        let composite_spec = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: Some(vec![fdf::ParentSpec { bind_rules: bind_rules, properties: properties }]),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(composite_spec.clone(), vec![])
        );

        // Match node.
        let mut device_properties: DeviceProperties = HashMap::new();
        device_properties.insert(
            PropertyKey::NumberKey(1), /* "fuchsia.BIND_PROTOCOL" */
            Symbol::NumberValue(200),
        );
        device_properties.insert(
            PropertyKey::StringKey("fuchsia.BIND_USB_PID".to_string()),
            Symbol::NumberValue(10),
        );

        let expected_parent = fdf::CompositeParent {
            composite: Some(fdf::CompositeInfo {
                spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
                ..Default::default()
            }),
            index: Some(0),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent])),
            composite_node_spec_manager.match_parent_specs(&device_properties)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_multiple_spec_match() {
        let bind_rules_2_rearranged = vec![
            make_accept(
                fdf::NodePropertyKey::StringValue("flycatcher".to_string()),
                fdf::NodePropertyValue::EnumValue("flycatcher.phoebe".to_string()),
            ),
            make_reject(
                fdf::NodePropertyKey::StringValue("killdeer".to_string()),
                fdf::NodePropertyValue::StringValue("plover".to_string()),
            ),
            make_reject(
                fdf::NodePropertyKey::StringValue("yellowlegs".to_string()),
                fdf::NodePropertyValue::BoolValue(true),
            ),
        ];

        let properties_2 = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let bind_rules_3 = vec![make_accept(
            fdf::NodePropertyKey::StringValue("cormorant".to_string()),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let properties_3 = vec![make_property(
            fdf::NodePropertyKey::StringValue("anhinga".to_string()),
            fdf::NodePropertyValue::BoolValue(false),
        )];

        let composite_spec_1 = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: Some(vec![create_test_parent_spec_1(), create_test_parent_spec_2()]),
            ..Default::default()
        };

        let composite_spec_2 = fdf::CompositeNodeSpec {
            name: Some("test_spec2".to_string()),
            parents: Some(vec![
                fdf::ParentSpec { bind_rules: bind_rules_2_rearranged, properties: properties_2 },
                fdf::ParentSpec { bind_rules: bind_rules_3, properties: properties_3 },
            ]),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(composite_spec_1.clone(), vec![])
        );

        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(composite_spec_2.clone(), vec![])
        );

        // Match node.
        let mut device_properties: DeviceProperties = HashMap::new();
        device_properties
            .insert(PropertyKey::StringKey("yellowlegs".to_string()), Symbol::BoolValue(false));
        device_properties.insert(
            PropertyKey::StringKey("killdeer".to_string()),
            Symbol::StringValue("lapwing".to_string()),
        );
        device_properties.insert(
            PropertyKey::StringKey("flycatcher".to_string()),
            Symbol::EnumValue("flycatcher.phoebe".to_string()),
        );
        let match_result =
            composite_node_spec_manager.match_parent_specs(&device_properties).unwrap();

        assert!(
            if let fdi::MatchDriverResult::CompositeParents(matched_node_info) = match_result {
                assert_eq!(2, matched_node_info.len());

                assert!(matched_node_info.contains(&fdf::CompositeParent {
                    composite: Some(fdf::CompositeInfo {
                        spec: Some(strip_parents_from_spec(&Some(composite_spec_1.clone()))),
                        ..Default::default()
                    }),
                    index: Some(1),
                    ..Default::default()
                }));

                assert!(matched_node_info.contains(&fdf::CompositeParent {
                    composite: Some(fdf::CompositeInfo {
                        spec: Some(strip_parents_from_spec(&Some(composite_spec_2.clone()))),
                        ..Default::default()
                    }),
                    index: Some(0),
                    ..Default::default()
                }));

                true
            } else {
                false
            }
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_multiple_spec_nodes_match() {
        let bind_rules_1 = vec![
            make_accept(fdf::NodePropertyKey::IntValue(1), fdf::NodePropertyValue::IntValue(200)),
            make_accept(
                fdf::NodePropertyKey::StringValue("killdeer".to_string()),
                fdf::NodePropertyValue::StringValue("plover".to_string()),
            ),
        ];

        let properties_1 = vec![make_property(
            fdf::NodePropertyKey::IntValue(2),
            fdf::NodePropertyValue::BoolValue(false),
        )];

        let bind_rules_1_rearranged = vec![
            make_accept(
                fdf::NodePropertyKey::StringValue("killdeer".to_string()),
                fdf::NodePropertyValue::StringValue("plover".to_string()),
            ),
            make_accept(fdf::NodePropertyKey::IntValue(1), fdf::NodePropertyValue::IntValue(200)),
        ];

        let bind_rules_3 = vec![make_accept(
            fdf::NodePropertyKey::StringValue("cormorant".to_string()),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let properties_3 = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(false),
        )];

        let bind_rules_4 = vec![make_accept_list(
            fdf::NodePropertyKey::IntValue(1),
            vec![fdf::NodePropertyValue::IntValue(10), fdf::NodePropertyValue::IntValue(200)],
        )];

        let properties_4 = vec![make_property(
            fdf::NodePropertyKey::IntValue(2),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let composite_spec_1 = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: Some(vec![
                fdf::ParentSpec { bind_rules: bind_rules_1, properties: properties_1.clone() },
                create_test_parent_spec_2(),
            ]),
            ..Default::default()
        };

        let composite_spec_2 = fdf::CompositeNodeSpec {
            name: Some("test_spec2".to_string()),
            parents: Some(vec![
                fdf::ParentSpec { bind_rules: bind_rules_3, properties: properties_3 },
                fdf::ParentSpec { bind_rules: bind_rules_1_rearranged, properties: properties_1 },
            ]),
            ..Default::default()
        };

        let composite_spec_3 = fdf::CompositeNodeSpec {
            name: Some("test_spec3".to_string()),
            parents: Some(vec![fdf::ParentSpec {
                bind_rules: bind_rules_4,
                properties: properties_4,
            }]),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(composite_spec_1.clone(), vec![])
        );

        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(composite_spec_2.clone(), vec![])
        );

        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(composite_spec_3.clone(), vec![])
        );

        // Match node.
        let mut device_properties: DeviceProperties = HashMap::new();
        device_properties.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(200));
        device_properties.insert(
            PropertyKey::StringKey("killdeer".to_string()),
            Symbol::StringValue("plover".to_string()),
        );
        let match_result =
            composite_node_spec_manager.match_parent_specs(&device_properties).unwrap();

        assert!(
            if let fdi::MatchDriverResult::CompositeParents(matched_node_info) = match_result {
                assert_eq!(3, matched_node_info.len());

                assert!(matched_node_info.contains(&fdf::CompositeParent {
                    composite: Some(fdf::CompositeInfo {
                        spec: Some(strip_parents_from_spec(&Some(composite_spec_1.clone()))),
                        ..Default::default()
                    }),
                    index: Some(0),
                    ..Default::default()
                }));

                assert!(matched_node_info.contains(&fdf::CompositeParent {
                    composite: Some(fdf::CompositeInfo {
                        spec: Some(strip_parents_from_spec(&Some(composite_spec_2.clone()))),
                        ..Default::default()
                    }),
                    index: Some(1),
                    ..Default::default()
                }));

                assert!(matched_node_info.contains(&fdf::CompositeParent {
                    composite: Some(fdf::CompositeInfo {
                        spec: Some(strip_parents_from_spec(&Some(composite_spec_3.clone()))),
                        ..Default::default()
                    }),
                    index: Some(0),
                    ..Default::default()
                }));

                true
            } else {
                false
            }
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_property_mismatch() {
        let bind_rules_2 = vec![
            make_accept(
                fdf::NodePropertyKey::StringValue("killdeer".to_string()),
                fdf::NodePropertyValue::StringValue("plover".to_string()),
            ),
            make_reject(
                fdf::NodePropertyKey::StringValue("yellowlegs".to_string()),
                fdf::NodePropertyValue::BoolValue(false),
            ),
        ];

        let properties_2 = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![
                        create_test_parent_spec_1(),
                        fdf::ParentSpec { bind_rules: bind_rules_2, properties: properties_2 },
                    ]),
                    ..Default::default()
                },
                vec![]
            )
        );

        let mut device_properties: DeviceProperties = HashMap::new();
        device_properties.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(200));
        device_properties.insert(
            PropertyKey::StringKey("kingfisher".to_string()),
            Symbol::StringValue("bee-eater".to_string()),
        );
        device_properties
            .insert(PropertyKey::StringKey("yellowlegs".to_string()), Symbol::BoolValue(false));
        device_properties.insert(
            PropertyKey::StringKey("killdeer".to_string()),
            Symbol::StringValue("plover".to_string()),
        );

        assert_eq!(None, composite_node_spec_manager.match_parent_specs(&device_properties));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_property_match_list() {
        let bind_rules_1 = vec![
            make_reject_list(
                fdf::NodePropertyKey::IntValue(10),
                vec![fdf::NodePropertyValue::IntValue(200), fdf::NodePropertyValue::IntValue(150)],
            ),
            make_accept_list(
                fdf::NodePropertyKey::StringValue("plover".to_string()),
                vec![
                    fdf::NodePropertyValue::StringValue("killdeer".to_string()),
                    fdf::NodePropertyValue::StringValue("lapwing".to_string()),
                ],
            ),
        ];

        let properties_1 = vec![make_property(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(100),
        )];

        let bind_rules_2 = vec![
            make_reject_list(
                fdf::NodePropertyKey::IntValue(11),
                vec![fdf::NodePropertyValue::IntValue(20), fdf::NodePropertyValue::IntValue(10)],
            ),
            make_accept(
                fdf::NodePropertyKey::StringValue("dunlin".to_string()),
                fdf::NodePropertyValue::BoolValue(true),
            ),
        ];

        let properties_2 = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let composite_spec = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: Some(vec![
                fdf::ParentSpec { bind_rules: bind_rules_1, properties: properties_1 },
                fdf::ParentSpec { bind_rules: bind_rules_2, properties: properties_2 },
            ]),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(composite_spec.clone(), vec![])
        );

        // Match node 1.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(10), Symbol::NumberValue(20));
        device_properties_1.insert(
            PropertyKey::StringKey("plover".to_string()),
            Symbol::StringValue("lapwing".to_string()),
        );

        let expected_parent_1 = fdf::CompositeParent {
            composite: Some(fdf::CompositeInfo {
                spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
                ..Default::default()
            }),
            index: Some(0),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent_1])),
            composite_node_spec_manager.match_parent_specs(&device_properties_1)
        );

        // Match node 2.
        let mut device_properties_2: DeviceProperties = HashMap::new();
        device_properties_2.insert(PropertyKey::NumberKey(5), Symbol::NumberValue(20));
        device_properties_2
            .insert(PropertyKey::StringKey("dunlin".to_string()), Symbol::BoolValue(true));

        let expected_parent_2 = fdf::CompositeParent {
            composite: Some(fdf::CompositeInfo {
                spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
                ..Default::default()
            }),
            index: Some(1),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent_2])),
            composite_node_spec_manager.match_parent_specs(&device_properties_2)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_property_mismatch_list() {
        let bind_rules_1 = vec![
            make_reject_list(
                fdf::NodePropertyKey::IntValue(10),
                vec![fdf::NodePropertyValue::IntValue(200), fdf::NodePropertyValue::IntValue(150)],
            ),
            make_accept_list(
                fdf::NodePropertyKey::StringValue("plover".to_string()),
                vec![
                    fdf::NodePropertyValue::StringValue("killdeer".to_string()),
                    fdf::NodePropertyValue::StringValue("lapwing".to_string()),
                ],
            ),
        ];

        let properties_1 = vec![make_property(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(100),
        )];

        let bind_rules_2 = vec![
            make_reject_list(
                fdf::NodePropertyKey::IntValue(11),
                vec![fdf::NodePropertyValue::IntValue(20), fdf::NodePropertyValue::IntValue(10)],
            ),
            make_accept(fdf::NodePropertyKey::IntValue(2), fdf::NodePropertyValue::BoolValue(true)),
        ];

        let properties_2 = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![
                        fdf::ParentSpec { bind_rules: bind_rules_1, properties: properties_1 },
                        fdf::ParentSpec { bind_rules: bind_rules_2, properties: properties_2 },
                    ]),
                    ..Default::default()
                },
                vec![]
            )
        );

        // Match node 1.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(10), Symbol::NumberValue(200));
        device_properties_1.insert(
            PropertyKey::StringKey("plover".to_string()),
            Symbol::StringValue("lapwing".to_string()),
        );
        assert_eq!(None, composite_node_spec_manager.match_parent_specs(&device_properties_1));

        // Match node 2.
        let mut device_properties_2: DeviceProperties = HashMap::new();
        device_properties_2.insert(PropertyKey::NumberKey(11), Symbol::NumberValue(10));
        device_properties_2.insert(PropertyKey::NumberKey(2), Symbol::BoolValue(true));

        assert_eq!(None, composite_node_spec_manager.match_parent_specs(&device_properties_2));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_property_multiple_value_types() {
        let bind_rules = vec![make_reject_list(
            fdf::NodePropertyKey::IntValue(10),
            vec![fdf::NodePropertyValue::IntValue(200), fdf::NodePropertyValue::BoolValue(false)],
        )];

        let properties = vec![make_property(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(100),
        )];

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::INVALID_ARGS.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![fdf::ParentSpec {
                        bind_rules: bind_rules,
                        properties: properties,
                    }]),
                    ..Default::default()
                },
                vec![]
            )
        );

        assert!(composite_node_spec_manager.parent_refs.is_empty());
        assert!(composite_node_spec_manager.spec_list.is_empty());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_property_duplicate_key() {
        let bind_rules = vec![
            make_reject_list(
                fdf::NodePropertyKey::IntValue(10),
                vec![fdf::NodePropertyValue::IntValue(200), fdf::NodePropertyValue::IntValue(150)],
            ),
            make_accept(fdf::NodePropertyKey::IntValue(10), fdf::NodePropertyValue::IntValue(10)),
        ];

        let properties = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::INVALID_ARGS.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![fdf::ParentSpec {
                        bind_rules: bind_rules,
                        properties: properties,
                    },]),
                    ..Default::default()
                },
                vec![]
            )
        );

        assert!(composite_node_spec_manager.parent_refs.is_empty());
        assert!(composite_node_spec_manager.spec_list.is_empty());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_missing_bind_rules() {
        let bind_rules = vec![
            make_reject_list(
                fdf::NodePropertyKey::IntValue(10),
                vec![fdf::NodePropertyValue::IntValue(200), fdf::NodePropertyValue::IntValue(150)],
            ),
            make_accept(fdf::NodePropertyKey::IntValue(10), fdf::NodePropertyValue::IntValue(10)),
        ];

        let properties_1 = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let properties_2 = vec![make_property(
            fdf::NodePropertyKey::IntValue(10),
            fdf::NodePropertyValue::BoolValue(false),
        )];

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::INVALID_ARGS.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![
                        fdf::ParentSpec { bind_rules: bind_rules, properties: properties_1 },
                        fdf::ParentSpec { bind_rules: vec![], properties: properties_2 },
                    ]),
                    ..Default::default()
                },
                vec![]
            )
        );

        assert!(composite_node_spec_manager.parent_refs.is_empty());
        assert!(composite_node_spec_manager.spec_list.is_empty());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_missing_composite_node_spec_fields() {
        let bind_rules = vec![
            make_reject_list(
                fdf::NodePropertyKey::IntValue(10),
                vec![fdf::NodePropertyValue::IntValue(200), fdf::NodePropertyValue::IntValue(150)],
            ),
            make_accept(fdf::NodePropertyKey::IntValue(10), fdf::NodePropertyValue::IntValue(10)),
        ];

        let properties_1 = vec![make_property(
            fdf::NodePropertyKey::IntValue(3),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let properties_2 = vec![make_property(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::BoolValue(false),
        )];

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::INVALID_ARGS.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: None,
                    parents: Some(vec![
                        fdf::ParentSpec { bind_rules: bind_rules, properties: properties_1 },
                        fdf::ParentSpec { bind_rules: vec![], properties: properties_2 },
                    ]),
                    ..Default::default()
                },
                vec![]
            )
        );
        assert!(composite_node_spec_manager.parent_refs.is_empty());
        assert!(composite_node_spec_manager.spec_list.is_empty());

        assert_eq!(
            Err(Status::INVALID_ARGS.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: None,
                    ..Default::default()
                },
                vec![]
            )
        );

        assert!(composite_node_spec_manager.parent_refs.is_empty());
        assert!(composite_node_spec_manager.spec_list.is_empty());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_match() {
        let primary_bind_rules = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(200),
        )];

        let additional_bind_rules_1 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(10),
        )];

        let additional_bind_rules_2 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(10),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let primary_key_1 = "whimbrel";
        let primary_val_1 = "sanderling";

        let additional_a_key_1 = 100;
        let additional_a_val_1 = 50;

        let additional_b_key_1 = "curlew";
        let additional_b_val_1 = 500;

        let primary_parent_spec = fdf::ParentSpec {
            bind_rules: primary_bind_rules,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(primary_key_1.to_string()),
                fdf::NodePropertyValue::StringValue(primary_val_1.to_string()),
            )],
        };

        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(primary_key_1.to_string(), ValueType::Str),
                rhs: Symbol::StringValue(primary_val_1.to_string()),
            },
        }];

        let additional_parent_spec_a = fdf::ParentSpec {
            bind_rules: additional_bind_rules_1,
            properties: vec![make_property(
                fdf::NodePropertyKey::IntValue(additional_a_key_1),
                fdf::NodePropertyValue::IntValue(additional_a_val_1),
            )],
        };

        let additional_node_a_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::DeprecatedKey(additional_a_key_1),
                    rhs: Symbol::NumberValue(additional_a_val_1.clone().into()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfEqual {
                    lhs: Symbol::Key("NA".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(500),
                },
            },
        ];

        let additional_parent_spec_b = fdf::ParentSpec {
            bind_rules: additional_bind_rules_2,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(additional_b_key_1.to_string()),
                fdf::NodePropertyValue::IntValue(additional_b_val_1),
            )],
        };

        let additional_node_b_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(additional_b_key_1.to_string(), ValueType::Number),
                rhs: Symbol::NumberValue(additional_b_val_1.clone().into()),
            },
        }];

        let composite_driver = create_driver_with_rules(
            (TEST_PRIMARY_NAME, primary_node_inst),
            vec![
                (TEST_ADDITIONAL_A_NAME, additional_node_a_inst),
                (TEST_ADDITIONAL_B_NAME, additional_node_b_inst),
            ],
            vec![],
        );

        let nodes =
            Some(vec![primary_parent_spec, additional_parent_spec_b, additional_parent_spec_a]);

        let composite_spec = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: nodes.clone(),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager
                .add_composite_node_spec(composite_spec.clone(), vec![&composite_driver])
        );

        assert_eq!(1, composite_node_spec_manager.get_specs(None).len());
        assert_eq!(0, composite_node_spec_manager.get_specs(Some("not_there".to_string())).len());
        let specs = composite_node_spec_manager.get_specs(Some("test_spec".to_string()));
        assert_eq!(1, specs.len());
        let composite_node_spec = &specs[0];

        let expected_spec = fdf::CompositeInfo {
            spec: Some(composite_spec.clone()),
            matched_driver: Some(fdf::CompositeDriverMatch {
                composite_driver: Some(fdf::CompositeDriverInfo {
                    composite_name: Some(TEST_DEVICE_NAME.to_string()),
                    driver_info: Some(composite_driver.clone().create_driver_info(false)),
                    ..Default::default()
                }),
                parent_names: Some(vec![
                    TEST_PRIMARY_NAME.to_string(),
                    TEST_ADDITIONAL_B_NAME.to_string(),
                    TEST_ADDITIONAL_A_NAME.to_string(),
                ]),
                primary_parent_index: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        };

        let expected_spec_stripped_parents = fdf::CompositeInfo {
            spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
            matched_driver: Some(fdf::CompositeDriverMatch {
                composite_driver: Some(fdf::CompositeDriverInfo {
                    composite_name: Some(TEST_DEVICE_NAME.to_string()),
                    driver_info: Some(composite_driver.clone().create_driver_info(false)),
                    ..Default::default()
                }),
                parent_names: Some(vec![
                    TEST_PRIMARY_NAME.to_string(),
                    TEST_ADDITIONAL_B_NAME.to_string(),
                    TEST_ADDITIONAL_A_NAME.to_string(),
                ]),
                primary_parent_index: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(&expected_spec, composite_node_spec);

        // Match additional node A, the last node in the spec at index 2.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(10));

        let expected_parent = fdf::CompositeParent {
            composite: Some(expected_spec_stripped_parents.clone()),
            index: Some(2),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent])),
            composite_node_spec_manager.match_parent_specs(&device_properties_1)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_with_rearranged_primary_node() {
        let primary_bind_rules = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(200),
        )];

        let additional_bind_rules_1 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(10),
        )];

        let additional_bind_rules_2 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(10),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let primary_key_1 = "whimbrel";
        let primary_val_1 = "sanderling";

        let additional_a_key_1 = 100;
        let additional_a_val_1 = 50;

        let additional_b_key_1 = "curlew";
        let additional_b_val_1 = 500;

        let primary_parent_spec = fdf::ParentSpec {
            bind_rules: primary_bind_rules,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(primary_key_1.to_string()),
                fdf::NodePropertyValue::StringValue(primary_val_1.to_string()),
            )],
        };

        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(primary_key_1.to_string(), ValueType::Str),
                rhs: Symbol::StringValue(primary_val_1.to_string()),
            },
        }];

        let additional_parent_spec_a = fdf::ParentSpec {
            bind_rules: additional_bind_rules_1,
            properties: vec![make_property(
                fdf::NodePropertyKey::IntValue(additional_a_key_1),
                fdf::NodePropertyValue::IntValue(additional_a_val_1),
            )],
        };

        let additional_node_a_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::DeprecatedKey(additional_a_key_1),
                    rhs: Symbol::NumberValue(additional_a_val_1.clone().into()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfEqual {
                    lhs: Symbol::Key("NA".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(500),
                },
            },
        ];

        let additional_parent_spec_b = fdf::ParentSpec {
            bind_rules: additional_bind_rules_2,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(additional_b_key_1.to_string()),
                fdf::NodePropertyValue::IntValue(additional_b_val_1),
            )],
        };

        let additional_node_b_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(additional_b_key_1.to_string(), ValueType::Number),
                rhs: Symbol::NumberValue(additional_b_val_1.clone().into()),
            },
        }];

        let composite_driver = create_driver_with_rules(
            (TEST_PRIMARY_NAME, primary_node_inst),
            vec![
                (TEST_ADDITIONAL_A_NAME, additional_node_a_inst),
                (TEST_ADDITIONAL_B_NAME, additional_node_b_inst),
            ],
            vec![],
        );

        let nodes =
            Some(vec![additional_parent_spec_b, additional_parent_spec_a, primary_parent_spec]);

        let composite_spec = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: nodes.clone(),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager
                .add_composite_node_spec(composite_spec.clone(), vec![&composite_driver])
        );

        assert_eq!(1, composite_node_spec_manager.get_specs(None).len());
        assert_eq!(0, composite_node_spec_manager.get_specs(Some("not_there".to_string())).len());
        let specs = composite_node_spec_manager.get_specs(Some("test_spec".to_string()));
        assert_eq!(1, specs.len());
        let composite_node_spec = &specs[0];

        let expected_spec = fdf::CompositeInfo {
            spec: Some(composite_spec.clone()),
            matched_driver: Some(fdf::CompositeDriverMatch {
                composite_driver: Some(fdf::CompositeDriverInfo {
                    composite_name: Some(TEST_DEVICE_NAME.to_string()),
                    driver_info: Some(composite_driver.clone().create_driver_info(false)),
                    ..Default::default()
                }),
                parent_names: Some(vec![
                    TEST_ADDITIONAL_B_NAME.to_string(),
                    TEST_ADDITIONAL_A_NAME.to_string(),
                    TEST_PRIMARY_NAME.to_string(),
                ]),
                primary_parent_index: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };

        let expected_spec_stripped_parents = fdf::CompositeInfo {
            spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
            matched_driver: Some(fdf::CompositeDriverMatch {
                composite_driver: Some(fdf::CompositeDriverInfo {
                    composite_name: Some(TEST_DEVICE_NAME.to_string()),
                    driver_info: Some(composite_driver.clone().create_driver_info(false)),
                    ..Default::default()
                }),
                parent_names: Some(vec![
                    TEST_ADDITIONAL_B_NAME.to_string(),
                    TEST_ADDITIONAL_A_NAME.to_string(),
                    TEST_PRIMARY_NAME.to_string(),
                ]),
                primary_parent_index: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(&expected_spec, composite_node_spec);

        // Match additional node A, the last node in the spec at index 2.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(10));

        let expected_parent = fdf::CompositeParent {
            composite: Some(expected_spec_stripped_parents.clone()),
            index: Some(1),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent])),
            composite_node_spec_manager.match_parent_specs(&device_properties_1)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_with_optional_match_without_optional() {
        let primary_bind_rules = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(200),
        )];

        let additional_bind_rules_1 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(10),
        )];

        let additional_bind_rules_2 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(10),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let primary_key_1 = "whimbrel";
        let primary_val_1 = "sanderling";

        let additional_a_key_1 = 100;
        let additional_a_val_1 = 50;

        let additional_b_key_1 = "curlew";
        let additional_b_val_1 = 500;

        let optional_a_key_1 = 200;
        let optional_a_val_1: u32 = 10;

        let primary_parent_spec = fdf::ParentSpec {
            bind_rules: primary_bind_rules,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(primary_key_1.to_string()),
                fdf::NodePropertyValue::StringValue(primary_val_1.to_string()),
            )],
        };

        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(primary_key_1.to_string(), ValueType::Str),
                rhs: Symbol::StringValue(primary_val_1.to_string()),
            },
        }];

        let additional_parent_spec_a = fdf::ParentSpec {
            bind_rules: additional_bind_rules_1,
            properties: vec![make_property(
                fdf::NodePropertyKey::IntValue(additional_a_key_1),
                fdf::NodePropertyValue::IntValue(additional_a_val_1),
            )],
        };

        let additional_node_a_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::DeprecatedKey(additional_a_key_1),
                    rhs: Symbol::NumberValue(additional_a_val_1.clone().into()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfEqual {
                    lhs: Symbol::Key("NA".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(500),
                },
            },
        ];

        let additional_parent_spec_b = fdf::ParentSpec {
            bind_rules: additional_bind_rules_2,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(additional_b_key_1.to_string()),
                fdf::NodePropertyValue::IntValue(additional_b_val_1),
            )],
        };

        let additional_node_b_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(additional_b_key_1.to_string(), ValueType::Number),
                rhs: Symbol::NumberValue(additional_b_val_1.clone().into()),
            },
        }];

        let optional_node_a_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::DeprecatedKey(optional_a_key_1),
                    rhs: Symbol::NumberValue(optional_a_val_1.clone().into()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfEqual {
                    lhs: Symbol::Key("NA".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(500),
                },
            },
        ];

        let composite_driver = create_driver_with_rules(
            (TEST_PRIMARY_NAME, primary_node_inst),
            vec![
                (TEST_ADDITIONAL_A_NAME, additional_node_a_inst),
                (TEST_ADDITIONAL_B_NAME, additional_node_b_inst),
            ],
            vec![(TEST_OPTIONAL_NAME, optional_node_a_inst)],
        );

        let composite_spec = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: Some(vec![
                primary_parent_spec,
                additional_parent_spec_b,
                additional_parent_spec_a,
            ]),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager
                .add_composite_node_spec(composite_spec.clone(), vec![&composite_driver])
        );

        // Match additional node A, the last node in the spec at index 2.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(10));

        let expected_parent = fdf::CompositeParent {
            composite: Some(fdf::CompositeInfo {
                spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
                matched_driver: Some(fdf::CompositeDriverMatch {
                    composite_driver: Some(fdf::CompositeDriverInfo {
                        composite_name: Some(TEST_DEVICE_NAME.to_string()),
                        driver_info: Some(composite_driver.clone().create_driver_info(false)),
                        ..Default::default()
                    }),
                    parent_names: Some(vec![
                        TEST_PRIMARY_NAME.to_string(),
                        TEST_ADDITIONAL_B_NAME.to_string(),
                        TEST_ADDITIONAL_A_NAME.to_string(),
                    ]),
                    primary_parent_index: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            index: Some(2),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent])),
            composite_node_spec_manager.match_parent_specs(&device_properties_1)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_with_optional_match_with_optional() {
        let primary_bind_rules = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(200),
        )];

        let additional_bind_rules_1 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(10),
        )];

        let additional_bind_rules_2 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(10),
            fdf::NodePropertyValue::BoolValue(true),
        )];

        let optional_bind_rules_1 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1000),
            fdf::NodePropertyValue::IntValue(1000),
        )];

        let primary_key_1 = "whimbrel";
        let primary_val_1 = "sanderling";

        let additional_a_key_1 = 100;
        let additional_a_val_1 = 50;

        let additional_b_key_1 = "curlew";
        let additional_b_val_1 = 500;

        let optional_a_key_1 = 200;
        let optional_a_val_1 = 10;

        let primary_parent_spec = fdf::ParentSpec {
            bind_rules: primary_bind_rules,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(primary_key_1.to_string()),
                fdf::NodePropertyValue::StringValue(primary_val_1.to_string()),
            )],
        };

        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(primary_key_1.to_string(), ValueType::Str),
                rhs: Symbol::StringValue(primary_val_1.to_string()),
            },
        }];

        let additional_parent_spec_a = fdf::ParentSpec {
            bind_rules: additional_bind_rules_1,
            properties: vec![make_property(
                fdf::NodePropertyKey::IntValue(additional_a_key_1),
                fdf::NodePropertyValue::IntValue(additional_a_val_1),
            )],
        };

        let additional_node_a_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::DeprecatedKey(additional_a_key_1),
                    rhs: Symbol::NumberValue(additional_a_val_1.clone().into()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfEqual {
                    lhs: Symbol::Key("NA".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(500),
                },
            },
        ];

        let additional_parent_spec_b = fdf::ParentSpec {
            bind_rules: additional_bind_rules_2,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(additional_b_key_1.to_string()),
                fdf::NodePropertyValue::IntValue(additional_b_val_1),
            )],
        };

        let additional_node_b_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(additional_b_key_1.to_string(), ValueType::Number),
                rhs: Symbol::NumberValue(additional_b_val_1.clone().into()),
            },
        }];

        let optional_node_parent_a = fdf::ParentSpec {
            bind_rules: optional_bind_rules_1,
            properties: vec![make_property(
                fdf::NodePropertyKey::IntValue(optional_a_key_1),
                fdf::NodePropertyValue::IntValue(optional_a_val_1),
            )],
        };

        let optional_node_a_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::DeprecatedKey(optional_a_key_1),
                    rhs: Symbol::NumberValue(optional_a_val_1.clone().into()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfEqual {
                    lhs: Symbol::Key("NA".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(500),
                },
            },
        ];

        let composite_driver = create_driver_with_rules(
            (TEST_PRIMARY_NAME, primary_node_inst),
            vec![
                (TEST_ADDITIONAL_A_NAME, additional_node_a_inst),
                (TEST_ADDITIONAL_B_NAME, additional_node_b_inst),
            ],
            vec![(TEST_OPTIONAL_NAME, optional_node_a_inst)],
        );

        let composite_spec = fdf::CompositeNodeSpec {
            name: Some("test_spec".to_string()),
            parents: Some(vec![
                primary_parent_spec,
                additional_parent_spec_b,
                optional_node_parent_a,
                additional_parent_spec_a,
            ]),
            ..Default::default()
        };

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager
                .add_composite_node_spec(composite_spec.clone(), vec![&composite_driver])
        );

        // Match additional node A, the last node in the spec at index 3.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(10));

        let expected_composite = fdf::CompositeInfo {
            spec: Some(strip_parents_from_spec(&Some(composite_spec.clone()))),
            matched_driver: Some(fdf::CompositeDriverMatch {
                composite_driver: Some(fdf::CompositeDriverInfo {
                    composite_name: Some(TEST_DEVICE_NAME.to_string()),
                    driver_info: Some(composite_driver.clone().create_driver_info(false)),
                    ..Default::default()
                }),
                parent_names: Some(vec![
                    TEST_PRIMARY_NAME.to_string(),
                    TEST_ADDITIONAL_B_NAME.to_string(),
                    TEST_OPTIONAL_NAME.to_string(),
                    TEST_ADDITIONAL_A_NAME.to_string(),
                ]),
                primary_parent_index: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        };

        let expected_parent = fdf::CompositeParent {
            composite: Some(expected_composite.clone()),
            index: Some(3),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent])),
            composite_node_spec_manager.match_parent_specs(&device_properties_1)
        );

        // Match optional node A, the second to last node in the spec at index 2.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1000), Symbol::NumberValue(1000));

        let expected_parent_2 = fdf::CompositeParent {
            composite: Some(expected_composite.clone()),
            index: Some(2),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchDriverResult::CompositeParents(vec![expected_parent_2])),
            composite_node_spec_manager.match_parent_specs(&device_properties_1)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_composite_mismatch() {
        let primary_bind_rules = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(200),
        )];

        let additional_bind_rules_1 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(10),
        )];

        let additional_bind_rules_2 = vec![make_accept(
            fdf::NodePropertyKey::IntValue(10),
            fdf::NodePropertyValue::BoolValue(false),
        )];

        let primary_key_1 = "whimbrel";
        let primary_val_1 = "sanderling";

        let additional_a_key_1 = 100;
        let additional_a_val_1 = 50;

        let additional_b_key_1 = "curlew";
        let additional_b_val_1 = 500;

        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(primary_key_1.to_string(), ValueType::Str),
                rhs: Symbol::StringValue(primary_val_1.to_string()),
            },
        }];

        let primary_parent_spec = fdf::ParentSpec {
            bind_rules: primary_bind_rules,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(primary_key_1.to_string()),
                fdf::NodePropertyValue::StringValue(primary_val_1.to_string()),
            )],
        };

        let additional_node_a_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key(additional_b_key_1.to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(additional_b_val_1.clone().into()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                // This does not exist in our properties so we expect it to not match.
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("NA".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(500),
                },
            },
        ];

        let additional_parent_spec_a = fdf::ParentSpec {
            bind_rules: additional_bind_rules_1,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(additional_b_key_1.to_string()),
                fdf::NodePropertyValue::IntValue(additional_b_val_1),
            )],
        };

        let additional_node_b_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::DeprecatedKey(additional_a_key_1.clone()),
                rhs: Symbol::NumberValue(additional_a_val_1.clone().into()),
            },
        }];

        let additional_parent_spec_b = fdf::ParentSpec {
            bind_rules: additional_bind_rules_2,
            properties: vec![make_property(
                fdf::NodePropertyKey::IntValue(additional_a_key_1),
                fdf::NodePropertyValue::IntValue(additional_a_val_1),
            )],
        };

        let composite_driver = create_driver_with_rules(
            (TEST_PRIMARY_NAME, primary_node_inst),
            vec![
                (TEST_ADDITIONAL_A_NAME, additional_node_a_inst),
                (TEST_ADDITIONAL_B_NAME, additional_node_b_inst),
            ],
            vec![],
        );

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![
                        primary_parent_spec,
                        additional_parent_spec_a,
                        additional_parent_spec_b
                    ]),
                    ..Default::default()
                },
                vec![&composite_driver]
            )
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_valid_name() {
        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();

        let node = fdf::ParentSpec {
            bind_rules: vec![make_accept(
                fdf::NodePropertyKey::StringValue("wrybill".to_string()),
                fdf::NodePropertyValue::IntValue(200),
            )],
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue("dotteral".to_string()),
                fdf::NodePropertyValue::StringValue("wrybill".to_string()),
            )],
        };
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test-spec".to_string()),
                    parents: Some(vec![node.clone()]),
                    ..Default::default()
                },
                vec![]
            )
        );

        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![node]),
                    ..Default::default()
                },
                vec![]
            )
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_invalid_name() {
        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        let node = fdf::ParentSpec {
            bind_rules: vec![make_accept(
                fdf::NodePropertyKey::StringValue("wrybill".to_string()),
                fdf::NodePropertyValue::IntValue(200),
            )],
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue("dotteral".to_string()),
                fdf::NodePropertyValue::IntValue(100),
            )],
        };
        assert_eq!(
            Err(Status::INVALID_ARGS.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test/spec".to_string()),
                    parents: Some(vec![node.clone()]),
                    ..Default::default()
                },
                vec![]
            )
        );

        assert_eq!(
            Err(Status::INVALID_ARGS.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test:spec".to_string()),
                    parents: Some(vec![node]),
                    ..Default::default()
                },
                vec![]
            )
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_rebind() {
        let primary_bind_rules = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(200),
        )];

        let primary_key_1 = "whimbrel";
        let primary_val_1 = "sanderling";

        let primary_parent_spec = fdf::ParentSpec {
            bind_rules: primary_bind_rules,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(primary_key_1.to_string()),
                fdf::NodePropertyValue::StringValue(primary_val_1.to_string()),
            )],
        };

        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(primary_key_1.to_string(), ValueType::Str),
                rhs: Symbol::StringValue(primary_val_1.to_string()),
            },
        }];

        let composite_driver = create_driver_with_rules(
            (TEST_PRIMARY_NAME, primary_node_inst.clone()),
            vec![],
            vec![],
        );

        let nodes = Some(vec![primary_parent_spec]);

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: nodes.clone(),
                    ..Default::default()
                },
                vec![&composite_driver]
            )
        );

        let rebind_driver = create_driver(
            "rebind_composite".to_string(),
            (TEST_PRIMARY_NAME, primary_node_inst),
            vec![],
            vec![],
        );
        assert!(composite_node_spec_manager
            .rebind("test_spec".to_string(), vec![&rebind_driver])
            .is_ok());
        assert_eq!(
            fdf::CompositeDriverInfo {
                composite_name: Some("rebind_composite".to_string()),
                driver_info: Some(rebind_driver.clone().create_driver_info(false)),
                ..Default::default()
            },
            composite_node_spec_manager
                .spec_list
                .get("test_spec")
                .unwrap()
                .matched_driver
                .as_ref()
                .unwrap()
                .composite_driver
                .as_ref()
                .unwrap()
                .clone()
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_rebind_no_match() {
        let primary_bind_rules = vec![make_accept(
            fdf::NodePropertyKey::IntValue(1),
            fdf::NodePropertyValue::IntValue(200),
        )];

        let primary_key_1 = "whimbrel";
        let primary_val_1 = "sanderling";

        let primary_parent_spec = fdf::ParentSpec {
            bind_rules: primary_bind_rules,
            properties: vec![make_property(
                fdf::NodePropertyKey::StringValue(primary_key_1.to_string()),
                fdf::NodePropertyValue::StringValue(primary_val_1.to_string()),
            )],
        };

        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key(primary_key_1.to_string(), ValueType::Str),
                rhs: Symbol::StringValue(primary_val_1.to_string()),
            },
        }];

        let composite_driver =
            create_driver_with_rules((TEST_PRIMARY_NAME, primary_node_inst), vec![], vec![]);

        let nodes = Some(vec![primary_parent_spec]);

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok(()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: nodes.clone(),
                    ..Default::default()
                },
                vec![&composite_driver]
            )
        );

        // Create a composite driver for rebinding that won't match to the spec.
        let rebind_primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("unmatched".to_string(), ValueType::Bool),
                rhs: Symbol::BoolValue(false),
            },
        }];

        let rebind_driver = create_driver(
            "rebind_composite".to_string(),
            (TEST_PRIMARY_NAME, rebind_primary_node_inst),
            vec![],
            vec![],
        );
        assert!(composite_node_spec_manager
            .rebind("test_spec".to_string(), vec![&rebind_driver])
            .is_ok());
        assert_eq!(
            None,
            composite_node_spec_manager.spec_list.get("test_spec").unwrap().matched_driver
        );
    }
}
