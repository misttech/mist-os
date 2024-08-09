// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::dict_ext::DictExt;
use crate::bedrock::request_metadata;
use crate::capability_source::{
    CapabilitySource, CapabilityToCapabilitySource, ComponentCapability, ComponentSource,
};
use crate::component_instance::ComponentInstanceInterface;
use crate::{RouteRequest, RoutingError};
use cm_rust::FidlIntoNative;
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

fn source_to_value(
    default: &Option<cm_rust::ConfigValue>,
    source: CapabilitySource,
) -> Result<Option<cm_rust::ConfigValue>, RoutingError> {
    let moniker = source.source_moniker();
    let cap = match source {
        CapabilitySource::Void(_) => {
            return Ok(default.clone());
        }
        CapabilitySource::Capability(CapabilityToCapabilitySource {
            source_capability, ..
        }) => source_capability,
        CapabilitySource::Component(ComponentSource { capability, .. }) => capability,
        o => {
            return Err(RoutingError::unsupported_route_source(moniker, o.type_name().to_string()));
        }
    };

    let cap = match cap {
        ComponentCapability::Config(c) => c,
        c => {
            return Err(RoutingError::unsupported_capability_type(moniker, c.type_name()));
        }
    };
    Ok(Some(cap.value))
}

pub async fn route_config_value_with_bedrock<C>(
    use_config: &cm_rust::UseConfigurationDecl,
    component: &Arc<C>,
) -> Result<Option<cm_rust::ConfigValue>, router_error::RouterError>
where
    C: ComponentInstanceInterface + 'static,
{
    let component_sandbox =
        component.component_sandbox().await.map_err(|e| RoutingError::from(e))?;
    let capability =
        match component_sandbox.program_input.config.get_capability(&use_config.target_name) {
            Some(c) => c,
            None => {
                return Err(RoutingError::BedrockNotPresentInDictionary {
                    name: use_config.target_name.to_string(),
                    moniker: component.moniker().clone(),
                }
                .into());
            }
        };
    let sandbox::Capability::Router(router) = capability else {
        return Err(RoutingError::BedrockWrongCapabilityType {
            actual: format!("{:?}", capability),
            expected: "Router".to_string(),
            moniker: component.moniker().clone().into(),
        }
        .into());
    };
    let request = sandbox::Request {
        availability: use_config.availability,
        target: component.as_weak().into(),
        debug: false,
        metadata: request_metadata::config_metadata(),
    };
    let data = match router.route(request).await? {
        sandbox::Capability::Data(d) => d,
        sandbox::Capability::Unit(_) => return Ok(use_config.default.clone()),
        other => {
            return Err(RoutingError::BedrockWrongCapabilityType {
                actual: format!("{:?}", other),
                expected: "Data or Unit".to_string(),
                moniker: component.moniker().clone().into(),
            }
            .into());
        }
    };
    let sandbox::Data::Bytes(bytes) = data else {
        return Err(RoutingError::BedrockWrongCapabilityType {
            actual: format!("{:?}", data),
            expected: "Data::bytes".to_string(),
            moniker: component.moniker().clone().into(),
        }
        .into());
    };
    let config_value: fidl_fuchsia_component_decl::ConfigValue = match fidl::unpersist(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return Err(RoutingError::BedrockWrongCapabilityType {
                actual: "{unknown}".into(),
                expected: "fuchsia.component.decl.ConfigValue".into(),
                moniker: component.moniker().clone().into(),
            }
            .into())
        }
    };

    Ok(Some(config_value.fidl_into_native()))
}

/// Route the given `use_config` from a specific `component`.
/// This returns the configuration value as a result.
/// This will return Ok(None) if it was routed successfully, but it
/// was an optional capability.
pub async fn route_config_value<C>(
    use_config: &cm_rust::UseConfigurationDecl,
    component: &Arc<C>,
) -> Result<Option<cm_rust::ConfigValue>, router_error::RouterError>
where
    C: ComponentInstanceInterface + 'static,
{
    if let Ok(Some(value)) = route_config_value_with_bedrock(use_config, component).await {
        return Ok(Some(value));
    }
    let source = crate::route_capability(
        RouteRequest::UseConfig(use_config.clone()),
        component,
        &mut crate::mapper::NoopRouteMapper,
    )
    .await?;
    Ok(source_to_value(&use_config.default, source.source)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_source::VoidSource;
    use moniker::Moniker;

    #[test]
    fn config_from_void() {
        let void_source = CapabilitySource::Void(VoidSource {
            capability: crate::capability_source::InternalCapability::Config(
                "test".parse().unwrap(),
            ),
            moniker: Moniker::root(),
        });
        assert_eq!(Ok(None), source_to_value(&None, void_source));
    }

    #[test]
    fn config_from_capability() {
        let test_value: cm_rust::ConfigValue = cm_rust::ConfigSingleValue::Uint8(5).into();
        let void_source = CapabilitySource::Capability(CapabilityToCapabilitySource {
            source_capability: crate::capability_source::ComponentCapability::Config(
                cm_rust::ConfigurationDecl {
                    name: "test".parse().unwrap(),
                    value: test_value.clone(),
                },
            ),
            moniker: Moniker::root(),
        });
        assert_eq!(Ok(Some(test_value)), source_to_value(&None, void_source));
    }

    #[test]
    fn config_from_component() {
        let test_value: cm_rust::ConfigValue = cm_rust::ConfigSingleValue::Uint8(5).into();
        let void_source = CapabilitySource::Component(ComponentSource {
            capability: crate::capability_source::ComponentCapability::Config(
                cm_rust::ConfigurationDecl {
                    name: "test".parse().unwrap(),
                    value: test_value.clone(),
                },
            ),
            moniker: Moniker::root(),
        });
        assert_eq!(Ok(Some(test_value)), source_to_value(&None, void_source));
    }
}
