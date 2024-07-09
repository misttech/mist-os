// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod controller;

#[cfg(test)]
mod tests {
    use scrutiny::engine::dispatcher::ControllerDispatcher;
    use scrutiny::engine::manager::PluginManager;
    use scrutiny::engine::scheduler::CollectorScheduler;
    use scrutiny::prelude::*;
    use std::sync::{Arc, Mutex, RwLock};

    use crate::core::collection::{Component, ComponentSource, Components};
    use crate::engine::controller::collector::{
        CollectorListController, CollectorListEntry, CollectorSchedulerController,
    };
    use crate::engine::controller::controller::ControllerListController;
    use crate::engine::controller::model::{
        ModelConfigController, ModelStats, ModelStatsController,
    };
    use crate::engine::controller::plugin::{PluginListController, PluginListEntry};
    use anyhow::Result;
    use scrutiny::plugin;
    use scrutiny_testing::fake::*;
    use serde_json::json;
    use std::collections::HashSet;
    use uuid::Uuid;

    plugin!(FakePlugin, PluginHooks::new(collectors! {}, controllers! {}), vec![]);

    struct FakeCollector {}
    impl DataCollector for FakeCollector {
        fn collect(&self, _model: Arc<DataModel>) -> Result<()> {
            Ok(())
        }
    }

    fn data_model() -> Arc<DataModel> {
        fake_data_model()
    }

    fn dispatcher(model: Arc<DataModel>) -> Arc<RwLock<ControllerDispatcher>> {
        Arc::new(RwLock::new(ControllerDispatcher::new(model)))
    }

    fn scheduler(model: Arc<DataModel>) -> Arc<Mutex<CollectorScheduler>> {
        Arc::new(Mutex::new(CollectorScheduler::new(model)))
    }

    fn plugin_manager(model: Arc<DataModel>) -> Arc<Mutex<PluginManager>> {
        let dispatcher = dispatcher(model.clone());
        let scheduler = scheduler(model.clone());
        Arc::new(Mutex::new(PluginManager::new(scheduler, dispatcher)))
    }

    #[fuchsia::test]
    fn test_plugin_list_controller() {
        let model = data_model();
        let manager = plugin_manager(model.clone());
        manager.lock().unwrap().register(Box::new(FakePlugin::new())).unwrap();
        let plugin_list = PluginListController::new(Arc::downgrade(&manager));
        let response = plugin_list.query(model.clone(), json!("")).unwrap();
        let list: Vec<PluginListEntry> = serde_json::from_value(response).unwrap();
        assert_eq!(list.len(), 1);
    }

    #[fuchsia::test]
    fn test_model_stats_controller() {
        let model = data_model();
        let model_stats = ModelStatsController::default();
        assert_eq!(model_stats.query(model.clone(), json!("")).is_ok(), true);
        model
            .set(Components::new(vec![Component {
                id: 1,
                url: cm_types::Url::new("fuchsia-pkg://fuchsia.com/test#meta/test.cm").unwrap(),
                source: ComponentSource::ZbiBootfs,
            }]))
            .unwrap();
        let response = model_stats.query(model.clone(), json!("")).unwrap();
        let stats: ModelStats = serde_json::from_value(response).unwrap();
        assert_eq!(stats.components, 1);
        assert_eq!(stats.packages, 0);
        assert_eq!(stats.manifests, 0);
        assert_eq!(stats.zbi_sections, 0);
        assert_eq!(stats.bootfs_files, 0);
    }

    #[fuchsia::test]
    fn test_model_env_controller() {
        let model = data_model();
        let model_stats = ModelConfigController::default();
        assert_eq!(model_stats.query(model.clone(), json!("")).is_ok(), true);
    }

    #[fuchsia::test]
    fn test_controller_list_controller() {
        let model = data_model();
        let dispatcher = dispatcher(model.clone());
        dispatcher
            .write()
            .unwrap()
            .add(Uuid::new_v4(), "/foo/bar".to_string(), Arc::new(ModelStatsController::default()))
            .unwrap();
        let controller_list = ControllerListController::new(dispatcher);
        let response = controller_list.query(model.clone(), json!("")).unwrap();
        let controllers: Vec<String> = serde_json::from_value(response).unwrap();
        assert_eq!(controllers, vec!["/foo/bar".to_string()]);
    }

    #[fuchsia::test]
    fn test_collector_list_controller() {
        let model = data_model();
        let scheduler = scheduler(model.clone());
        scheduler.lock().unwrap().add(
            Uuid::new_v4(),
            "foo",
            HashSet::new(),
            Arc::new(FakeCollector {}),
        );
        let collector_list = CollectorListController::new(scheduler);
        let response = collector_list.query(model.clone(), json!("")).unwrap();
        let list: Vec<CollectorListEntry> = serde_json::from_value(response).unwrap();
        assert_eq!(list.len(), 1);
    }

    #[fuchsia::test]
    fn test_collector_scheduler_controller() {
        let model = data_model();
        let scheduler = scheduler(model.clone());
        let schedule_controller = CollectorSchedulerController::new(scheduler);
        let response = schedule_controller.query(model.clone(), json!("")).unwrap();
        assert_eq!(response, json!({"status": "ok"}));
    }
}
