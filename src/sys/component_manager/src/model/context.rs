// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::error::ModelError,
    ::routing::policy::GlobalPolicyChecker,
    cm_config::{AbiRevisionPolicy, RuntimeConfig},
    std::sync::Arc,
};

/// The ModelContext provides the API boundary between the Model and Realms. It
/// defines what parts of the Model or authoritative state about the tree we
/// want to share with Realms.
pub struct ModelContext {
    component_id_index: component_id_index::Index,
    policy_checker: GlobalPolicyChecker,
    runtime_config: Arc<RuntimeConfig>,
}

impl ModelContext {
    /// Constructs a new ModelContext from a RuntimeConfig.
    pub fn new(runtime_config: Arc<RuntimeConfig>) -> Result<Self, ModelError> {
        Ok(Self {
            component_id_index: match &runtime_config.component_id_index_path {
                Some(path) => component_id_index::Index::from_fidl_file(&path)?,
                None => component_id_index::Index::default(),
            },
            policy_checker: GlobalPolicyChecker::new(runtime_config.security_policy.clone()),
            runtime_config,
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

    pub fn component_id_index(&self) -> &component_id_index::Index {
        &self.component_id_index
    }

    pub fn abi_revision_policy(&self) -> &AbiRevisionPolicy {
        &self.runtime_config.abi_revision_policy
    }
}
