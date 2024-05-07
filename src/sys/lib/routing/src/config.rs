// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability_source::CapabilitySource;
use crate::capability_source::ComponentCapability;
use crate::component_instance::ComponentInstanceInterface;
use crate::RouteRequest;
use crate::RoutingError;
use std::sync::Arc;

/// Get a specific configuration use declaration from the structured
/// config key value.
pub fn get_use_config_from_key<'a>(
    key: &str,
    decl: &'a cm_rust::ComponentDecl,
) -> Option<&'a cm_rust::UseConfigurationDecl> {
    decl.uses.iter().find_map(|use_| match use_ {
        cm_rust::UseDecl::Config(c) => (c.target_name == key).then_some(c),
        _ => None,
    })
}

fn source_to_value<C>(
    default: &Option<cm_rust::ConfigValue>,
    source: CapabilitySource<C>,
) -> Result<Option<cm_rust::ConfigValue>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let cap = match source {
        CapabilitySource::Void { .. } => {
            return Ok(default.clone());
        }
        CapabilitySource::Capability { source_capability, .. } => source_capability,
        CapabilitySource::Component { capability, .. } => capability,
        o => {
            return Err(RoutingError::UnsupportedRouteSource {
                source_type: o.type_name().to_string(),
            });
        }
    };

    let cap = match cap {
        ComponentCapability::Config(c) => c,
        c => {
            return Err(RoutingError::UnsupportedCapabilityType {
                type_name: c.type_name().into(),
            });
        }
    };
    Ok(Some(cap.value))
}

/// Route the given `use_config` from a specific `component`.
/// This returns the configuration value as a result.
/// This will return Ok(None) if it was routed successfully, but it
/// was an optional capability.
pub async fn route_config_value<C>(
    use_config: &cm_rust::UseConfigurationDecl,
    component: &Arc<C>,
) -> Result<Option<cm_rust::ConfigValue>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
{
    let source = crate::route_capability(
        RouteRequest::UseConfig(use_config.clone()),
        component,
        &mut crate::mapper::NoopRouteMapper,
    )
    .await?;
    source_to_value(&use_config.default, source.source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component_instance::tests::TestComponent;
    use crate::component_instance::WeakComponentInstanceInterface;

    #[test]
    fn config_from_void() {
        let void_source = CapabilitySource::Void {
            capability: crate::capability_source::InternalCapability::Config(
                "test".parse().unwrap(),
            ),
            component: WeakComponentInstanceInterface::<TestComponent>::invalid(),
        };
        assert_eq!(Ok(None), source_to_value(&None, void_source));
    }

    #[test]
    fn config_from_capability() {
        let test_value: cm_rust::ConfigValue = cm_rust::ConfigSingleValue::Uint8(5).into();
        let void_source = CapabilitySource::Capability {
            source_capability: crate::capability_source::ComponentCapability::Config(
                cm_rust::ConfigurationDecl {
                    name: "test".parse().unwrap(),
                    value: test_value.clone(),
                },
            ),
            component: WeakComponentInstanceInterface::<TestComponent>::invalid(),
        };
        assert_eq!(Ok(Some(test_value)), source_to_value(&None, void_source));
    }

    #[test]
    fn config_from_component() {
        let test_value: cm_rust::ConfigValue = cm_rust::ConfigSingleValue::Uint8(5).into();
        let void_source = CapabilitySource::Component {
            capability: crate::capability_source::ComponentCapability::Config(
                cm_rust::ConfigurationDecl {
                    name: "test".parse().unwrap(),
                    value: test_value.clone(),
                },
            ),
            component: WeakComponentInstanceInterface::<TestComponent>::invalid(),
        };
        assert_eq!(Ok(Some(test_value)), source_to_value(&None, void_source));
    }
}
