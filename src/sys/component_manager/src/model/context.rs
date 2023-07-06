// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::error::ModelError,
    ::routing::{
        component_id_index::ComponentIdIndex,
        config::{AbiRevisionPolicy, RuntimeConfig},
        policy::GlobalPolicyChecker,
    },
    std::sync::Arc,
};

/// The ModelContext provides the API boundary between the Model and Realms. It
/// defines what parts of the Model or authoritative state about the tree we
/// want to share with Realms.
pub struct ModelContext {
    policy_checker: GlobalPolicyChecker,
    component_id_index: Arc<ComponentIdIndex>,
    runtime_config: Arc<RuntimeConfig>,
    abi_revision_policy: AbiRevisionPolicy,
}

impl ModelContext {
    /// Constructs a new ModelContext from a RuntimeConfig.
    pub fn new(runtime_config: Arc<RuntimeConfig>) -> Result<Self, ModelError> {
        Ok(Self {
            component_id_index: match &runtime_config.component_id_index_path {
                Some(path) => Arc::new(ComponentIdIndex::new(&path)?),
                None => Arc::new(ComponentIdIndex::default()),
            },
            abi_revision_policy: runtime_config.abi_revision_policy.clone(),
            runtime_config: runtime_config.clone(),
            policy_checker: GlobalPolicyChecker::new(runtime_config.security_policy.clone()),
        })
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        let runtime_config = Arc::new(RuntimeConfig::default());
        Self::new(runtime_config).unwrap()
    }

    /// Returns the runtime policy checker for the model.
    pub fn policy(&self) -> &GlobalPolicyChecker {
        &self.policy_checker
    }

    pub fn runtime_config(&self) -> &Arc<RuntimeConfig> {
        &self.runtime_config
    }

    pub fn component_id_index(&self) -> Arc<ComponentIdIndex> {
        self.component_id_index.clone()
    }

    pub fn abi_revision_policy(&self) -> &AbiRevisionPolicy {
        &self.abi_revision_policy
    }
}
