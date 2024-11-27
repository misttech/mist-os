// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Weak;

use async_trait::async_trait;
use cm_rust::FidlIntoNative;
use cm_types::Name;
use fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker, ServerEnd};
use futures::StreamExt;
use lazy_static::lazy_static;
use moniker::Moniker;
use routing::capability_source::InternalCapability;
use tracing::warn;
use {fidl_fuchsia_component_decl as fcdecl, fidl_fuchsia_sys2 as fsys};

use crate::capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider};
use crate::model::component::WeakComponentInstance;
use crate::model::model::Model;

lazy_static! {
    static ref CAPABILITY_NAME: Name = fsys::ConfigOverrideMarker::PROTOCOL_NAME.parse().unwrap();
}

/// A type which implements the fuchsia.sys2.ConfigOverride FIDL protocol.
#[derive(Clone)]
pub struct ConfigOverride {
    /// A reference to the state of the component tree.
    model: Weak<Model>,
}

impl ConfigOverride {
    pub fn new(model: Weak<Model>) -> Self {
        Self { model }
    }

    async fn serve(self, scope_moniker: Moniker, mut stream: fsys::ConfigOverrideRequestStream) {
        while let Some(Ok(request)) = stream.next().await {
            let Some(model) = self.model.upgrade() else {
                break;
            };
            let result = match request {
                fsys::ConfigOverrideRequest::SetStructuredConfig { moniker, fields, responder } => {
                    let fields = fields.into_iter().map(FidlIntoNative::fidl_into_native).collect();
                    let result =
                        set_structured_config(&model, &scope_moniker, &moniker, fields).await;
                    responder.send(result)
                }
                fsys::ConfigOverrideRequest::UnsetStructuredConfig { moniker, responder } => {
                    let result = unset_structured_config(&model, &scope_moniker, &moniker).await;
                    responder.send(result)
                }
                fsys::ConfigOverrideRequest::_UnknownMethod { ordinal, method_type, .. } => {
                    warn!("{} received request for unknown method with ordinal {ordinal} and method type {method_type:?}", fsys::ConfigOverrideMarker::DEBUG_NAME);
                    Ok(())
                }
            };
            if let Err(error) = result {
                warn!(?error, "Could not respond to ConfigOverride request");
                break;
            }
        }
    }
}

impl FrameworkCapability for ConfigOverride {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CAPABILITY_NAME)
    }

    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        _target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(ConfigOverrideCapabilityProvider {
            config_override: self.clone(),
            scope_moniker: scope.moniker,
        })
    }
}

/// A wrapper around [`ConfigOverride`] providing the additional state needed to
/// make it compatible with fuchsia.io Open calls.
struct ConfigOverrideCapabilityProvider {
    /// An implementation of the fuchsia.sys2.ConfigOverride protocol.
    config_override: ConfigOverride,
    /// The moniker for the component tree realm to which the open request will
    /// be scoped.
    scope_moniker: Moniker,
}

#[async_trait]
impl InternalCapabilityProvider for ConfigOverrideCapabilityProvider {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fsys::ConfigOverrideMarker>::new(server_end);
        self.config_override.serve(self.scope_moniker, server_end.into_stream()).await;
    }
}

async fn set_structured_config(
    model: &Model,
    scope_moniker: &Moniker,
    moniker: &str,
    fields: Vec<cm_rust::ConfigOverride>,
) -> Result<(), fsys::ConfigOverrideError> {
    // Construct the complete moniker using the scope moniker and the moniker string.
    let moniker = Moniker::try_from(moniker).map_err(|_| fsys::ConfigOverrideError::BadMoniker)?;
    let moniker = scope_moniker.concat(&moniker);

    let instance =
        model.root().find(&moniker).await.ok_or(fsys::ConfigOverrideError::InstanceNotFound)?;
    let state = instance.lock_state().await;
    let config: fcdecl::ResolvedConfig = state
        .get_resolved_state()
        .ok_or(fsys::ConfigOverrideError::InstanceNotResolved)?
        .config()
        .ok_or(fsys::ConfigOverrideError::NoConfig)?
        .clone()
        .into();
    for field in fields {
        // Verify a field with this key has been declared for the component.
        config
            .fields
            .iter()
            .find(|f| *f.key == field.key)
            .ok_or(fsys::ConfigOverrideError::KeyNotFound)?;
        model.context().add_config_developer_override(moniker.clone(), field).await;
    }
    Ok(())
}

async fn unset_structured_config(
    model: &Model,
    scope_moniker: &Moniker,
    moniker: &str,
) -> Result<(), fsys::ConfigOverrideError> {
    if moniker.is_empty() {
        return Ok(model.context().clear_config_developer_override(&scope_moniker).await);
    }

    // Construct the complete moniker using the scope moniker and the moniker string.
    let moniker = Moniker::try_from(moniker).map_err(|_| fsys::ConfigOverrideError::BadMoniker)?;
    let moniker = scope_moniker.concat(&moniker);

    // Verify that the instance specified by moniker exists.
    let _instance =
        model.root().find(&moniker).await.ok_or(fsys::ConfigOverrideError::InstanceNotFound)?;
    model
        .context()
        .remove_config_developer_override(&moniker)
        .await
        .map_err(|_e| fsys::ConfigOverrideError::NoConfig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::test_helpers::{
        config_override, lifecycle_controller, new_config_decl, TestEnvironmentBuilder,
    };
    use cm_rust::{ConfigSingleValue, ConfigValue, NativeIntoFidl};
    use cm_rust_testing::*;

    #[fuchsia::test]
    async fn set_structured_config_test() {
        let (config, config_values, _checksum) = new_config_decl();
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("has_config").eager())
                    .child(ChildBuilder::new().name("no_config").eager())
                    .build(),
            ),
            ("has_config", ComponentDeclBuilder::new().config(config).build()),
            ("no_config", ComponentDeclBuilder::new().build()),
        ];

        let test_model_result = TestEnvironmentBuilder::new()
            .set_components(components)
            .set_config_values(vec![("meta/root.cvf", config_values)])
            .build()
            .await;
        let config_override_proxy = config_override(&test_model_result).await;
        let lifecycle_controller_proxy = lifecycle_controller(&test_model_result).await;

        lifecycle_controller_proxy.resolve_instance(".").await.unwrap().unwrap();
        lifecycle_controller_proxy.resolve_instance("./has_config").await.unwrap().unwrap();
        lifecycle_controller_proxy.resolve_instance("./no_config").await.unwrap().unwrap();

        assert_eq!(
            config_override_proxy.set_structured_config("", &[]).await.unwrap(),
            Err(fsys::ConfigOverrideError::BadMoniker)
        );

        assert_eq!(
            config_override_proxy.set_structured_config("./doesnotexist", &[]).await.unwrap(),
            Err(fsys::ConfigOverrideError::InstanceNotFound)
        );

        lifecycle_controller_proxy.unresolve_instance("./has_config").await.unwrap().unwrap();
        assert_eq!(
            config_override_proxy.set_structured_config("./has_config", &[]).await.unwrap(),
            Err(fsys::ConfigOverrideError::InstanceNotResolved)
        );
        lifecycle_controller_proxy.resolve_instance("./has_config").await.unwrap().unwrap();

        assert_eq!(
            config_override_proxy.set_structured_config("./no_config", &[]).await.unwrap(),
            Err(fsys::ConfigOverrideError::NoConfig),
        );

        assert_eq!(
            config_override_proxy
                .set_structured_config(
                    "./has_config",
                    &[cm_rust::ConfigOverride {
                        key: String::from("bogus_key"),
                        value: ConfigValue::Single(ConfigSingleValue::Bool(true))
                    }
                    .native_into_fidl()]
                )
                .await
                .unwrap(),
            Err(fsys::ConfigOverrideError::KeyNotFound),
        );

        assert_eq!(
            config_override_proxy
                .set_structured_config(
                    "./has_config",
                    &[cm_rust::ConfigOverride {
                        key: String::from("my_field"),
                        value: ConfigValue::Single(ConfigSingleValue::Bool(false))
                    }
                    .native_into_fidl()]
                )
                .await
                .unwrap(),
            Ok(())
        );
    }

    #[fuchsia::test]
    async fn unset_structured_config_test() {
        let (config, config_values, _checksum) = new_config_decl();
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new().child(ChildBuilder::new().name("a").eager()).build(),
            ),
            ("a", ComponentDeclBuilder::new().config(config).build()),
        ];

        let test_model_result = TestEnvironmentBuilder::new()
            .set_components(components)
            .set_config_values(vec![("meta/root.cvf", config_values)])
            .build()
            .await;
        let config_override_proxy = config_override(&test_model_result).await;
        let lifecycle_controller_proxy = lifecycle_controller(&test_model_result).await;

        lifecycle_controller_proxy.resolve_instance(".").await.unwrap().unwrap();
        lifecycle_controller_proxy.resolve_instance("./a").await.unwrap().unwrap();
        config_override_proxy
            .set_structured_config(
                "./a",
                &[cm_rust::ConfigOverride {
                    key: String::from("my_field"),
                    value: ConfigValue::Single(ConfigSingleValue::Bool(false)),
                }
                .native_into_fidl()],
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            config_override_proxy.unset_structured_config("not:valid:moniker").await.unwrap(),
            Err(fsys::ConfigOverrideError::BadMoniker)
        );

        assert_eq!(
            config_override_proxy.unset_structured_config("./doesnotexist").await.unwrap(),
            Err(fsys::ConfigOverrideError::InstanceNotFound)
        );

        assert_eq!(config_override_proxy.unset_structured_config("./a").await.unwrap(), Ok(()));

        assert_eq!(
            config_override_proxy.unset_structured_config("./a").await.unwrap(),
            Err(fsys::ConfigOverrideError::NoConfig),
        );
    }
}
