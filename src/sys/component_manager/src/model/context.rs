// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::{BuiltinCapability, CapabilityProvider, FrameworkCapability};
use crate::model::component::WeakComponentInstance;
use crate::model::token::InstanceRegistry;
use ::routing::capability_source::{BuiltinSource, CapabilitySource, FrameworkSource};
use ::routing::component_instance::ComponentInstanceInterface;
use ::routing::policy::GlobalPolicyChecker;
use cm_config::{AbiRevisionPolicy, RuntimeConfig};
use errors::ModelError;
use futures::lock::Mutex;
use moniker::Moniker;
use std::collections::HashMap;
use std::sync::Arc;
use vfs::execution_scope::ExecutionScope;

/// The ModelContext provides the API boundary between the Model and Realms. It
/// defines what parts of the Model or authoritative state about the tree we
/// want to share with Realms.
pub struct ModelContext {
    component_id_index: component_id_index::Index,
    policy_checker: GlobalPolicyChecker,
    runtime_config: Arc<RuntimeConfig>,
    builtin_capabilities: Mutex<Option<Vec<Box<dyn BuiltinCapability>>>>,
    framework_capabilities: Mutex<Option<Vec<Box<dyn FrameworkCapability>>>>,
    instance_registry: Arc<InstanceRegistry>,
    config_developer_overrides: Mutex<HashMap<Moniker, HashMap<String, cm_rust::ConfigValue>>>,
    pub scope_factory: Box<dyn Fn() -> ExecutionScope + Send + Sync + 'static>,
}

impl ModelContext {
    /// Constructs a new ModelContext from a RuntimeConfig.
    pub fn new(
        runtime_config: Arc<RuntimeConfig>,
        instance_registry: Arc<InstanceRegistry>,
        #[cfg(test)] scope_factory: Option<Box<dyn Fn() -> ExecutionScope + Send + Sync + 'static>>,
    ) -> Result<Self, ModelError> {
        #[cfg(not(test))]
        let scope_factory = Box::new(|| ExecutionScope::new());
        #[cfg(test)]
        let scope_factory = scope_factory.unwrap_or_else(|| Box::new(|| ExecutionScope::new()));
        Ok(Self {
            component_id_index: match &runtime_config.component_id_index_path {
                Some(path) => component_id_index::Index::from_fidl_file(&path)?,
                None => component_id_index::Index::default(),
            },
            policy_checker: GlobalPolicyChecker::new(runtime_config.security_policy.clone()),
            runtime_config,
            builtin_capabilities: Mutex::new(None),
            framework_capabilities: Mutex::new(None),
            instance_registry,
            config_developer_overrides: Mutex::new(HashMap::new()),
            scope_factory,
        })
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        let runtime_config = Arc::new(RuntimeConfig::default());
        let instance_registry = InstanceRegistry::new();
        Self::new(runtime_config, instance_registry, None).unwrap()
    }

    /// Returns the runtime policy checker for the model.
    pub fn policy(&self) -> &GlobalPolicyChecker {
        &self.policy_checker
    }

    pub fn runtime_config(&self) -> &Arc<RuntimeConfig> {
        &self.runtime_config
    }

    pub fn component_id_index(&self) -> &component_id_index::Index {
        &self.component_id_index
    }

    pub fn abi_revision_policy(&self) -> &AbiRevisionPolicy {
        &self.runtime_config.abi_revision_policy
    }

    pub fn instance_registry(&self) -> &Arc<InstanceRegistry> {
        &self.instance_registry
    }

    pub async fn init_internal_capabilities(
        &self,
        b: Vec<Box<dyn BuiltinCapability>>,
        f: Vec<Box<dyn FrameworkCapability>>,
    ) {
        {
            let mut builtin_capabilities = self.builtin_capabilities.lock().await;
            assert!(builtin_capabilities.is_none(), "already initialized");
            *builtin_capabilities = Some(b);
        }
        {
            let mut framework_capabilities = self.framework_capabilities.lock().await;
            assert!(framework_capabilities.is_none(), "already initialized");
            *framework_capabilities = Some(f);
        }
    }

    #[cfg(test)]
    pub async fn add_framework_capability(&self, c: Box<dyn FrameworkCapability>) {
        // Internal capabilities added for a test should preempt existing ones that match the
        // same metadata.
        let mut framework_capabilities = self.framework_capabilities.lock().await;
        framework_capabilities.as_mut().unwrap().insert(0, c);
    }

    /// Adds a configuration override for the component identified by moniker.
    /// This override will take effect the next time the component is started.
    pub async fn add_config_developer_override(
        &self,
        moniker: Moniker,
        config_override: cm_rust::ConfigOverride,
    ) {
        self.config_developer_overrides
            .lock()
            .await
            .entry(moniker)
            .or_default()
            .insert(config_override.key, config_override.value);
    }

    /// Retrieves a map of any configuration overrides for the component
    /// identified by moniker.
    pub async fn get_config_developer_overrides(
        &self,
        moniker: &Moniker,
    ) -> HashMap<String, cm_rust::ConfigValue> {
        self.config_developer_overrides
            .lock()
            .await
            .get(&moniker)
            .cloned()
            .unwrap_or_else(HashMap::new)
    }

    /// Removes the configuration overrides for the component identified by
    /// moniker. Returns an error if no overrides have been defined for the
    /// component.
    pub async fn remove_config_developer_override(
        &self,
        moniker: &Moniker,
    ) -> Result<(), ModelError> {
        match self.config_developer_overrides.lock().await.remove(moniker) {
            Some(_) => Ok(()),
            None => Err(ModelError::instance_not_found(moniker.clone())),
        }
    }

    /// Removes configuration overrides defined for any component in the realm
    /// defined by scope_moniker.
    pub async fn clear_config_developer_override(&self, scope_moniker: &Moniker) {
        let scoped_components = self
            .config_developer_overrides
            .lock()
            .await
            .keys()
            .filter(|k| k.has_prefix(scope_moniker))
            .cloned()
            .collect::<Vec<_>>();
        for scoped_component in scoped_components {
            match self.remove_config_developer_override(&scoped_component).await {
                Ok(()) => (),
                Err(e) => tracing::warn!(
                    "config overrides not found for component {scoped_component}: {e}"
                ),
            }
        }
    }

    pub async fn find_internal_provider(
        &self,
        source: &CapabilitySource,
        target: WeakComponentInstance,
    ) -> Option<Box<dyn CapabilityProvider>> {
        match source {
            CapabilitySource::Builtin(BuiltinSource { capability, .. }) => {
                let builtin_capabilities = self.builtin_capabilities.lock().await;
                for c in builtin_capabilities.as_ref().expect("not initialized") {
                    if c.matches(capability) {
                        return Some(c.new_provider(target));
                    }
                }
                None
            }
            CapabilitySource::Framework(FrameworkSource { capability, moniker }) => {
                let framework_capabilities = self.framework_capabilities.lock().await;
                for c in framework_capabilities.as_ref().expect("not initialized") {
                    if c.matches(capability) {
                        let source_component =
                            target.upgrade().ok()?.find_absolute(moniker).await.ok()?.as_weak();
                        return Some(c.new_provider(source_component, target));
                    }
                }
                None
            }
            _ => None,
        }
    }
}
