// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::match_common::{get_composite_rules_from_composite_driver, node_to_device_property},
    crate::resolved_driver::ResolvedDriver,
    bind::compiler::symbol_table::{get_deprecated_key_identifier, get_deprecated_key_value},
    bind::compiler::Symbol,
    bind::interpreter::decode_bind_rules::DecodedRules,
    bind::interpreter::match_bind::{match_bind, DeviceProperties, MatchBindData, PropertyKey},
    fidl_fuchsia_driver_development as fdd, fidl_fuchsia_driver_framework as fdf,
    fidl_fuchsia_driver_index as fdi,
    fuchsia_zircon::{zx_status_t, Status},
    regex::Regex,
    std::collections::{BTreeMap, HashMap, HashSet},
};

const NAME_REGEX: &'static str = r"^[a-zA-Z0-9\-_]*$";

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct BindRuleCondition {
    condition: fdf::Condition,
    values: Vec<Symbol>,
}

type BindRules = BTreeMap<PropertyKey, BindRuleCondition>;

struct MatchedComposite {
    pub info: fdi::MatchedCompositeInfo,
    pub names: Vec<String>,
    pub primary_index: u32,
}

struct CompositeNodeSpecInfo {
    pub nodes: Vec<fdf::ParentSpec>,

    // The composite driver matched to the spec.
    pub matched: Option<MatchedComposite>,
}

// The CompositeNodeSpecManager struct is responsible of managing a list of specs
// for matching.
pub struct CompositeNodeSpecManager {
    // Maps a list of specs to the bind rules of their nodes. This is to handle multiple
    // specs that share a node with the same bind rules. Used for matching nodes.
    pub parent_specs: HashMap<BindRules, Vec<fdi::MatchedCompositeNodeSpecInfo>>,

    // Maps specs to the name. This list ensures that we don't add multiple specs with
    // the same name.
    spec_list: HashMap<String, CompositeNodeSpecInfo>,
}

impl CompositeNodeSpecManager {
    pub fn new() -> Self {
        CompositeNodeSpecManager { parent_specs: HashMap::new(), spec_list: HashMap::new() }
    }

    pub fn add_composite_node_spec(
        &mut self,
        spec: fdf::CompositeNodeSpec,
        composite_drivers: Vec<&ResolvedDriver>,
    ) -> fdi::DriverIndexAddCompositeNodeSpecResult {
        // Get and validate the name.
        let name = spec.name.ok_or(Status::INVALID_ARGS.into_raw())?;
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

        let parents = spec.parents.ok_or(Status::INVALID_ARGS.into_raw())?;

        if self.spec_list.contains_key(&name) {
            return Err(Status::ALREADY_EXISTS.into_raw());
        }

        if parents.is_empty() {
            return Err(Status::INVALID_ARGS.into_raw());
        }

        // Collect parent specs in a separate vector before adding them to the
        // CompositeNodeSpecManager. This is to ensure that we add the parent specs after
        // they're all verified to be valid.
        // TODO(fxb/105562): Update tests so that we can verify that properties exists in
        // each parent spec.
        let mut parent_specs: Vec<(BindRules, fdi::MatchedCompositeNodeSpecInfo)> = vec![];
        for (idx, parent) in parents.iter().enumerate() {
            let properties = convert_fidl_to_bind_rules(&parent.bind_rules)?;
            let composite_node_spec_info = fdi::MatchedCompositeNodeSpecInfo {
                name: Some(name.clone()),
                node_index: Some(idx as u32),
                num_nodes: Some(parents.len() as u32),
                ..Default::default()
            };

            parent_specs.push((properties, composite_node_spec_info));
        }

        // Add each parent spec and its composite node spec into the map.
        for (properties, spec_info) in parent_specs {
            self.parent_specs
                .entry(properties)
                .and_modify(|specs| specs.push(spec_info.clone()))
                .or_insert(vec![spec_info]);
        }

        for composite_driver in composite_drivers {
            let matched_composite = match_composite_properties(composite_driver, &parents)?;
            if let Some(matched_composite) = matched_composite {
                // Found a match so we can set this in our map.
                self.spec_list.insert(
                    name.clone(),
                    CompositeNodeSpecInfo {
                        nodes: parents,
                        matched: Some(MatchedComposite {
                            info: matched_composite.info.clone(),
                            names: matched_composite.names.clone(),
                            primary_index: matched_composite.primary_index,
                        }),
                    },
                );
                tracing::info!(
                    "Matched '{}' to composite node spec '{}'",
                    get_driver_url(&matched_composite),
                    name
                );
                return Ok((matched_composite.info, matched_composite.names));
            }
        }

        self.spec_list.insert(name, CompositeNodeSpecInfo { nodes: parents, matched: None });
        Err(Status::NOT_FOUND.into_raw())
    }

    // Match the given device properties to all the nodes. Returns a list of specs for all the
    // nodes that match.
    pub fn match_parent_specs(&self, properties: &DeviceProperties) -> Option<fdi::MatchedDriver> {
        let mut specs: Vec<fdi::MatchedCompositeNodeSpecInfo> = vec![];
        for (node_props, spec_list) in self.parent_specs.iter() {
            if match_node(&node_props, properties) {
                specs.extend_from_slice(spec_list.as_slice());
            }
        }

        if specs.is_empty() {
            return None;
        }

        // Put in the matched composite info for this spec that we have stored in
        // |spec_list|.
        let mut specs_result = vec![];
        for composite_node_spec in specs {
            if let Some(composite_node_spec) =
                self.composite_node_spec_add_composite_info(composite_node_spec)
            {
                specs_result.push(composite_node_spec);
            }
        }

        if specs_result.is_empty() {
            return None;
        }

        Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
            specs: Some(specs_result),
            ..Default::default()
        }))
    }

    pub fn new_driver_available(&mut self, resolved_driver: ResolvedDriver) {
        // Only composite drivers should be matched against composite node specs.
        if matches!(resolved_driver.bind_rules, DecodedRules::Normal(_)) {
            return;
        }

        for (name, spec) in self.spec_list.iter_mut() {
            if spec.matched.is_some() {
                continue;
            }
            let matched_composite_result =
                match_composite_properties(&resolved_driver, &spec.nodes);
            if let Ok(Some(matched_composite)) = matched_composite_result {
                tracing::info!(
                    "Matched '{}' to composite node spec '{}'",
                    get_driver_url(&matched_composite),
                    name
                );
                spec.matched = Some(matched_composite);
            }
        }
    }

    pub fn get_specs(&self, name_filter: Option<String>) -> Vec<fdd::CompositeNodeSpecInfo> {
        if let Some(name) = name_filter {
            match self.spec_list.get(&name) {
                Some(item) => return vec![to_composite_node_spec_info(&name, item)],
                None => return vec![],
            }
        };

        let specs = self
            .spec_list
            .iter()
            .map(|(name, composite_node_spec_info)| {
                to_composite_node_spec_info(name, composite_node_spec_info)
            })
            .collect::<Vec<_>>();

        return specs;
    }

    fn composite_node_spec_add_composite_info(
        &self,
        mut info: fdi::MatchedCompositeNodeSpecInfo,
    ) -> Option<fdi::MatchedCompositeNodeSpecInfo> {
        if let Some(name) = &info.name {
            let list_value = self.spec_list.get(name);
            if let Some(composite_node_spec) = list_value {
                // TODO(fxb/107371): Only return specs that have a matched composite.
                if let Some(matched) = &composite_node_spec.matched {
                    info.composite = Some(matched.info.clone());
                    info.node_names = Some(matched.names.clone());
                    info.primary_index = Some(matched.primary_index);
                }

                return Some(info);
            }
        }

        return None;
    }
}

fn to_composite_node_spec_info(
    name: &str,
    composite_node_spec_info: &CompositeNodeSpecInfo,
) -> fdd::CompositeNodeSpecInfo {
    match &composite_node_spec_info.matched {
        Some(matched_driver) => {
            let driver = match &matched_driver.info.driver_info {
                Some(driver_info) => driver_info.url.clone().or(driver_info.driver_url.clone()),
                None => None,
            };
            fdd::CompositeNodeSpecInfo {
                name: Some(name.to_string()),
                driver,
                primary_index: Some(matched_driver.primary_index),
                parent_names: Some(matched_driver.names.clone()),
                parents: Some(composite_node_spec_info.nodes.clone()),
                ..Default::default()
            }
        }
        None => fdd::CompositeNodeSpecInfo {
            name: Some(name.to_string()),
            parents: Some(composite_node_spec_info.nodes.clone()),
            ..Default::default()
        },
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

        let first_val = fidl_rule.values.first().ok_or(Status::INVALID_ARGS.into_raw())?;
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
) -> Result<Option<MatchedComposite>, i32> {
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
    let mut primary_index = 0;
    let mut primary_matches = false;
    for i in 0..parents.len() {
        primary_matches = node_matches_composite_driver(
            &parents[i],
            &composite.primary_node.instructions,
            &composite.symbol_table,
        );
        if primary_matches {
            primary_index = i as u32;
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
    // TODO(fxb/107176): Disallow ambiguity with spec matching. We should log
    // a warning and return false if a spec node matches with multiple composite
    // driver nodes, and vice versa.
    let mut unmatched_additional_indices =
        (0..composite.additional_nodes.len()).collect::<HashSet<_>>();
    let mut unmatched_optional_indices =
        (0..composite.optional_nodes.len()).collect::<HashSet<_>>();

    let mut names = vec![];

    for i in 0..parents.len() {
        if i == primary_index as usize {
            names.push(composite.symbol_table[&composite.primary_node.name_id].clone());
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

        names.push(matched_name.unwrap());
    }

    // If we didn't consume all of the additional nodes in the bind rules then this is not a match.
    if !unmatched_additional_indices.is_empty() {
        return Ok(None);
    }

    let info = fdi::MatchedCompositeInfo {
        composite_name: Some(composite.symbol_table[&composite.device_name_id].clone()),
        driver_info: Some(composite_driver.create_matched_driver_info()),
        ..Default::default()
    };
    return Ok(Some(MatchedComposite { info, names, primary_index }));
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

fn get_driver_url(composite: &MatchedComposite) -> String {
    if let Some(driver_info) = &composite.info.driver_info {
        if let Some(url) = &driver_info.driver_url {
            return url.to_string();
        }
    }
    "".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolved_driver::DriverPackageType;
    use bind::compiler::{
        CompiledBindRules, CompositeBindRules, CompositeNode, Symbol, SymbolicInstruction,
        SymbolicInstructionInfo,
    };
    use bind::interpreter::decode_bind_rules::DecodedRules;
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

    // TODO(fxb/120270): Update tests so that they use the test data functions more often.
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

    fn create_driver_with_rules<'a>(
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
            device_name: TEST_DEVICE_NAME.to_string(),
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
            component_url: url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm")
                .unwrap(),
            v1_driver_path: None,
            bind_rules: rules,
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_property_match_node() {
        let nodes = Some(vec![create_test_parent_spec_1(), create_test_parent_spec_2()]);

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::NOT_FOUND.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: nodes.clone(),
                    ..Default::default()
                },
                vec![]
            )
        );

        assert_eq!(1, composite_node_spec_manager.get_specs(None).len());
        assert_eq!(0, composite_node_spec_manager.get_specs(Some("not_there".to_string())).len());
        let specs = composite_node_spec_manager.get_specs(Some("test_spec".to_string()));
        assert_eq!(1, specs.len());
        let composite_node_spec = &specs[0];
        let expected_composite_node_spec = fdd::CompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            parents: nodes,
            ..Default::default()
        };

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

        let expected_composite_node_spec = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(0),
            num_nodes: Some(2),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec]),
                ..Default::default()
            })),
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

        let expected_composite_node_spec_2 = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(1),
            num_nodes: Some(2),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec_2]),
                ..Default::default()
            })),
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

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::NOT_FOUND.into_raw()),
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

        // Match node.
        let mut device_properties: DeviceProperties = HashMap::new();
        device_properties.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(200));

        let expected_composite_node_spec = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(0),
            num_nodes: Some(1),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec]),
                ..Default::default()
            })),
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

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::NOT_FOUND.into_raw()),
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

        let expected_composite_node_spec = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(0),
            num_nodes: Some(1),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec]),
                ..Default::default()
            })),
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

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::NOT_FOUND.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![create_test_parent_spec_1(), create_test_parent_spec_2(),]),
                    ..Default::default()
                },
                vec![]
            )
        );

        assert_eq!(
            Err(Status::NOT_FOUND.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec2".to_string()),
                    parents: Some(vec![
                        fdf::ParentSpec {
                            bind_rules: bind_rules_2_rearranged,
                            properties: properties_2,
                        },
                        fdf::ParentSpec { bind_rules: bind_rules_3, properties: properties_3 },
                    ]),
                    ..Default::default()
                },
                vec![]
            )
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

        assert!(if let fdi::MatchedDriver::ParentSpec(matched_node_info) = match_result {
            let matched_specs = matched_node_info.specs.unwrap();
            assert_eq!(2, matched_specs.len());

            assert!(matched_specs.contains(&fdi::MatchedCompositeNodeSpecInfo {
                name: Some("test_spec".to_string()),
                node_index: Some(1),
                num_nodes: Some(2),
                ..Default::default()
            }));

            assert!(matched_specs.contains(&fdi::MatchedCompositeNodeSpecInfo {
                name: Some("test_spec2".to_string()),
                node_index: Some(0),
                num_nodes: Some(2),
                ..Default::default()
            }));

            true
        } else {
            false
        });
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

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::NOT_FOUND.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![
                        fdf::ParentSpec {
                            bind_rules: bind_rules_1,
                            properties: properties_1.clone(),
                        },
                        create_test_parent_spec_2(),
                    ]),
                    ..Default::default()
                },
                vec![]
            )
        );

        assert_eq!(
            Err(Status::NOT_FOUND.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec2".to_string()),
                    parents: Some(vec![
                        fdf::ParentSpec { bind_rules: bind_rules_3, properties: properties_3 },
                        fdf::ParentSpec {
                            bind_rules: bind_rules_1_rearranged,
                            properties: properties_1,
                        },
                    ]),
                    ..Default::default()
                },
                vec![]
            )
        );

        assert_eq!(
            Err(Status::NOT_FOUND.into_raw()),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec3".to_string()),
                    parents: Some(vec![fdf::ParentSpec {
                        bind_rules: bind_rules_4,
                        properties: properties_4,
                    }]),
                    ..Default::default()
                },
                vec![]
            )
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

        assert!(if let fdi::MatchedDriver::ParentSpec(matched_node_info) = match_result {
            let matched_specs = matched_node_info.specs.unwrap();
            assert_eq!(3, matched_specs.len());

            assert!(matched_specs.contains(&fdi::MatchedCompositeNodeSpecInfo {
                name: Some("test_spec".to_string()),
                node_index: Some(0),
                num_nodes: Some(2),
                ..Default::default()
            }));

            assert!(matched_specs.contains(&fdi::MatchedCompositeNodeSpecInfo {
                name: Some("test_spec2".to_string()),
                node_index: Some(1),
                num_nodes: Some(2),
                ..Default::default()
            }));

            assert!(matched_specs.contains(&fdi::MatchedCompositeNodeSpecInfo {
                name: Some("test_spec3".to_string()),
                node_index: Some(0),
                num_nodes: Some(1),
                ..Default::default()
            }));

            true
        } else {
            false
        });
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
            Err(Status::NOT_FOUND.into_raw()),
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

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Err(Status::NOT_FOUND.into_raw()),
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
        device_properties_1.insert(PropertyKey::NumberKey(10), Symbol::NumberValue(20));
        device_properties_1.insert(
            PropertyKey::StringKey("plover".to_string()),
            Symbol::StringValue("lapwing".to_string()),
        );

        let expected_composite_node_spec_1 = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(0),
            num_nodes: Some(2),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec_1]),
                ..Default::default()
            })),
            composite_node_spec_manager.match_parent_specs(&device_properties_1)
        );

        // Match node 2.
        let mut device_properties_2: DeviceProperties = HashMap::new();
        device_properties_2.insert(PropertyKey::NumberKey(5), Symbol::NumberValue(20));
        device_properties_2
            .insert(PropertyKey::StringKey("dunlin".to_string()), Symbol::BoolValue(true));

        let expected_composite_node_spec_2 = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(1),
            num_nodes: Some(2),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec_2]),
                ..Default::default()
            })),
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
            Err(Status::NOT_FOUND.into_raw()),
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

        assert!(composite_node_spec_manager.parent_specs.is_empty());
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

        assert!(composite_node_spec_manager.parent_specs.is_empty());
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

        assert!(composite_node_spec_manager.parent_specs.is_empty());
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
        assert!(composite_node_spec_manager.parent_specs.is_empty());
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

        assert!(composite_node_spec_manager.parent_specs.is_empty());
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

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok((
                fdi::MatchedCompositeInfo {
                    composite_name: Some(TEST_DEVICE_NAME.to_string()),
                    driver_info: Some(composite_driver.clone().create_matched_driver_info()),
                    ..Default::default()
                },
                vec![
                    TEST_PRIMARY_NAME.to_string(),
                    TEST_ADDITIONAL_B_NAME.to_string(),
                    TEST_ADDITIONAL_A_NAME.to_string()
                ]
            )),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: nodes.clone(),
                    ..Default::default()
                },
                vec![&composite_driver]
            )
        );

        assert_eq!(1, composite_node_spec_manager.get_specs(None).len());
        assert_eq!(0, composite_node_spec_manager.get_specs(Some("not_there".to_string())).len());
        let specs = composite_node_spec_manager.get_specs(Some("test_spec".to_string()));
        assert_eq!(1, specs.len());
        let composite_node_spec = &specs[0];
        let expected_composite_node_spec = fdd::CompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            driver: Some("fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm".to_string()),
            primary_index: Some(0),
            parent_names: Some(vec![
                TEST_PRIMARY_NAME.to_string(),
                TEST_ADDITIONAL_B_NAME.to_string(),
                TEST_ADDITIONAL_A_NAME.to_string(),
            ]),
            parents: nodes,
            ..Default::default()
        };

        assert_eq!(&expected_composite_node_spec, composite_node_spec);

        // Match additional node A, the last node in the spec at index 2.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(10));

        let expected_composite_node_spec = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(2),
            num_nodes: Some(3),
            primary_index: Some(0),
            node_names: Some(vec![
                TEST_PRIMARY_NAME.to_string(),
                TEST_ADDITIONAL_B_NAME.to_string(),
                TEST_ADDITIONAL_A_NAME.to_string(),
            ]),
            composite: Some(fdi::MatchedCompositeInfo {
                composite_name: Some(TEST_DEVICE_NAME.to_string()),
                driver_info: Some(composite_driver.clone().create_matched_driver_info()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec]),
                ..Default::default()
            })),
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

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok((
                fdi::MatchedCompositeInfo {
                    composite_name: Some(TEST_DEVICE_NAME.to_string()),
                    driver_info: Some(composite_driver.clone().create_matched_driver_info()),
                    ..Default::default()
                },
                vec![
                    TEST_ADDITIONAL_B_NAME.to_string(),
                    TEST_ADDITIONAL_A_NAME.to_string(),
                    TEST_PRIMARY_NAME.to_string(),
                ]
            )),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: nodes.clone(),
                    ..Default::default()
                },
                vec![&composite_driver]
            )
        );

        assert_eq!(1, composite_node_spec_manager.get_specs(None).len());
        assert_eq!(0, composite_node_spec_manager.get_specs(Some("not_there".to_string())).len());
        let specs = composite_node_spec_manager.get_specs(Some("test_spec".to_string()));
        assert_eq!(1, specs.len());
        let composite_node_spec = &specs[0];
        let expected_composite_node_spec = fdd::CompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            driver: Some("fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm".to_string()),
            primary_index: Some(2),
            parent_names: Some(vec![
                TEST_ADDITIONAL_B_NAME.to_string(),
                TEST_ADDITIONAL_A_NAME.to_string(),
                TEST_PRIMARY_NAME.to_string(),
            ]),
            parents: nodes,
            ..Default::default()
        };

        assert_eq!(&expected_composite_node_spec, composite_node_spec);

        // Match additional node A, the last node in the spec at index 2.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(10));

        let expected_composite_node_spec = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(1),
            num_nodes: Some(3),
            primary_index: Some(2),
            node_names: Some(vec![
                TEST_ADDITIONAL_B_NAME.to_string(),
                TEST_ADDITIONAL_A_NAME.to_string(),
                TEST_PRIMARY_NAME.to_string(),
            ]),
            composite: Some(fdi::MatchedCompositeInfo {
                composite_name: Some(TEST_DEVICE_NAME.to_string()),
                driver_info: Some(composite_driver.clone().create_matched_driver_info()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec]),
                ..Default::default()
            })),
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

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok((
                fdi::MatchedCompositeInfo {
                    composite_name: Some(TEST_DEVICE_NAME.to_string()),
                    driver_info: Some(composite_driver.clone().create_matched_driver_info()),
                    ..Default::default()
                },
                vec![
                    TEST_PRIMARY_NAME.to_string(),
                    TEST_ADDITIONAL_B_NAME.to_string(),
                    TEST_ADDITIONAL_A_NAME.to_string()
                ]
            )),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![
                        primary_parent_spec,
                        additional_parent_spec_b,
                        additional_parent_spec_a,
                    ]),
                    ..Default::default()
                },
                vec![&composite_driver]
            )
        );

        // Match additional node A, the last node in the spec at index 2.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(10));

        let expected_composite_node_spec = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(2),
            num_nodes: Some(3),
            primary_index: Some(0),
            node_names: Some(vec![
                TEST_PRIMARY_NAME.to_string(),
                TEST_ADDITIONAL_B_NAME.to_string(),
                TEST_ADDITIONAL_A_NAME.to_string(),
            ]),
            composite: Some(fdi::MatchedCompositeInfo {
                composite_name: Some(TEST_DEVICE_NAME.to_string()),
                driver_info: Some(composite_driver.clone().create_matched_driver_info()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec]),
                ..Default::default()
            })),
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

        let mut composite_node_spec_manager = CompositeNodeSpecManager::new();
        assert_eq!(
            Ok((
                fdi::MatchedCompositeInfo {
                    composite_name: Some(TEST_DEVICE_NAME.to_string()),
                    driver_info: Some(composite_driver.clone().create_matched_driver_info()),
                    ..Default::default()
                },
                vec![
                    TEST_PRIMARY_NAME.to_string(),
                    TEST_ADDITIONAL_B_NAME.to_string(),
                    TEST_OPTIONAL_NAME.to_string(),
                    TEST_ADDITIONAL_A_NAME.to_string()
                ]
            )),
            composite_node_spec_manager.add_composite_node_spec(
                fdf::CompositeNodeSpec {
                    name: Some("test_spec".to_string()),
                    parents: Some(vec![
                        primary_parent_spec,
                        additional_parent_spec_b,
                        optional_node_parent_a,
                        additional_parent_spec_a,
                    ]),
                    ..Default::default()
                },
                vec![&composite_driver]
            )
        );

        // Match additional node A, the last node in the spec at index 3.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1), Symbol::NumberValue(10));

        let expected_composite_node_spec = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(3),
            num_nodes: Some(4),
            primary_index: Some(0),
            node_names: Some(vec![
                TEST_PRIMARY_NAME.to_string(),
                TEST_ADDITIONAL_B_NAME.to_string(),
                TEST_OPTIONAL_NAME.to_string(),
                TEST_ADDITIONAL_A_NAME.to_string(),
            ]),
            composite: Some(fdi::MatchedCompositeInfo {
                composite_name: Some(TEST_DEVICE_NAME.to_string()),
                driver_info: Some(composite_driver.clone().create_matched_driver_info()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec]),
                ..Default::default()
            })),
            composite_node_spec_manager.match_parent_specs(&device_properties_1)
        );

        // Match optional node A, the second to last node in the spec at index 2.
        let mut device_properties_1: DeviceProperties = HashMap::new();
        device_properties_1.insert(PropertyKey::NumberKey(1000), Symbol::NumberValue(1000));

        let expected_composite_node_spec = fdi::MatchedCompositeNodeSpecInfo {
            name: Some("test_spec".to_string()),
            node_index: Some(2),
            num_nodes: Some(4),
            primary_index: Some(0),
            node_names: Some(vec![
                TEST_PRIMARY_NAME.to_string(),
                TEST_ADDITIONAL_B_NAME.to_string(),
                TEST_OPTIONAL_NAME.to_string(),
                TEST_ADDITIONAL_A_NAME.to_string(),
            ]),
            composite: Some(fdi::MatchedCompositeInfo {
                composite_name: Some(TEST_DEVICE_NAME.to_string()),
                driver_info: Some(composite_driver.clone().create_matched_driver_info()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            Some(fdi::MatchedDriver::ParentSpec(fdi::MatchedCompositeNodeParentInfo {
                specs: Some(vec![expected_composite_node_spec]),
                ..Default::default()
            })),
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
            Err(Status::NOT_FOUND.into_raw()),
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
            Err(Status::NOT_FOUND.into_raw()),
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
            Err(Status::NOT_FOUND.into_raw()),
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
}
