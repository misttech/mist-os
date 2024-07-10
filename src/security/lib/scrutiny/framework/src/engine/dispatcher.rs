// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::controller::DataController;
use crate::model::model::DataModel;
use anyhow::{Error, Result};
use serde_json::value::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DispatcherError {
    #[error("namespace: {0} is already in use and cannot be bound")]
    NamespaceInUse(String),
    #[error("namespace: {0} does not exist, query failing.")]
    NamespaceDoesNotExist(String),
}

/// `ControllerInstance` holds all the additional book-keeping information
/// required to attribute `instance` ownership to a particular controller.
struct ControllerInstance {
    pub controller: Arc<dyn DataController>,
}

/// The ControllerDispatcher provides a 1:1 mapping between namespaces and
/// unique DataController instances.
pub struct ControllerDispatcher {
    model: Arc<DataModel>,
    controllers: RwLock<HashMap<String, ControllerInstance>>,
}

impl ControllerDispatcher {
    pub fn new(model: Arc<DataModel>) -> Self {
        Self { model: model, controllers: RwLock::new(HashMap::new()) }
    }

    /// Adding a control will fail if there is a namespace collision. A
    /// namespace should reflect the REST API url e.g "components/manifests"
    pub fn add(&mut self, namespace: String, controller: Arc<dyn DataController>) -> Result<()> {
        let mut controllers = self.controllers.write().unwrap();
        if controllers.contains_key(&namespace) {
            return Err(Error::new(DispatcherError::NamespaceInUse(namespace)));
        }
        controllers.insert(namespace, ControllerInstance { controller });
        Ok(())
    }

    /// Attempts to service the query if the namespace has a mapping.
    pub fn query(&self, namespace: String, query: Value) -> Result<Value> {
        let controllers = self.controllers.read().unwrap();
        if let Some(instance) = controllers.get(&namespace) {
            instance.controller.query(Arc::clone(&self.model), query)
        } else {
            Err(Error::new(DispatcherError::NamespaceDoesNotExist(namespace)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scrutiny_testing::fake::fake_model_config;
    use serde_json::json;

    struct FakeController {
        pub result: String,
    }

    impl FakeController {
        pub fn new(result: impl Into<String>) -> Self {
            Self { result: result.into() }
        }
    }

    impl DataController for FakeController {
        fn query(&self, _: Arc<DataModel>, _: Value) -> Result<Value> {
            Ok(json!(self.result))
        }
    }

    fn test_model() -> Arc<DataModel> {
        Arc::new(DataModel::new(fake_model_config()).unwrap())
    }

    #[test]
    fn test_query() {
        let data_model = test_model();
        let mut dispatcher = ControllerDispatcher::new(data_model);
        let fake = Arc::new(FakeController::new("fake_result"));
        let namespace = "/foo/bar".to_string();
        dispatcher.add(namespace.clone(), fake).unwrap();
        assert_eq!(dispatcher.query(namespace, json!("")).unwrap(), json!("fake_result"));
    }

    #[test]
    fn test_query_multiple() {
        let data_model = test_model();
        let mut dispatcher = ControllerDispatcher::new(data_model);
        let fake = Arc::new(FakeController::new("fake_result"));
        let fake_two = Arc::new(FakeController::new("fake_result_two"));
        let namespace = "/foo/bar".to_string();
        let namespace_two = "/foo/baz".to_string();
        dispatcher.add(namespace.clone(), fake).unwrap();
        dispatcher.add(namespace_two.clone(), fake_two).unwrap();
        assert_eq!(dispatcher.query(namespace, json!("")).unwrap(), json!("fake_result"));
        assert_eq!(dispatcher.query(namespace_two, json!("")).unwrap(), json!("fake_result_two"));
    }
}
