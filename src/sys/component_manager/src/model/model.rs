// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        actions::{ActionKey, ActionSet, DiscoverAction},
        component::{manager::ComponentManagerInstance, ComponentInstance, StartReason},
        context::ModelContext,
        environment::Environment,
        start::Start,
        structured_dict::ComponentInput,
        token::InstanceRegistry,
    },
    cm_config::RuntimeConfig,
    cm_types::Url,
    errors::ModelError,
    std::sync::Arc,
    tracing::warn,
};

/// Parameters for initializing a component model, particularly the root of the component
/// instance tree.
pub struct ModelParams {
    // TODO(viktard): Merge into RuntimeConfig
    /// The URL of the root component.
    pub root_component_url: Url,
    /// The environment provided to the root.
    pub root_environment: Environment,
    /// Global runtime configuration for the component_manager.
    pub runtime_config: Arc<RuntimeConfig>,
    /// The instance at the top of the tree, representing component manager.
    pub top_instance: Arc<ComponentManagerInstance>,
}

/// The component model holds authoritative state about a tree of component instances, including
/// each instance's identity, lifecycle, capabilities, and topological relationships.  It also
/// provides operations for instantiating, destroying, querying, and controlling component
/// instances at runtime.
pub struct Model {
    /// The instance at the top of the tree, i.e. the instance representing component manager
    /// itself.
    top_instance: Arc<ComponentManagerInstance>,
    /// The instance representing the root component. Owned by `top_instance`, but cached here for
    /// efficiency.
    root: Arc<ComponentInstance>,
    /// The context shared across the model.
    context: Arc<ModelContext>,
}

impl Model {
    /// Creates a new component model and initializes its topology.
    pub async fn new(
        params: ModelParams,
        instance_registry: Arc<InstanceRegistry>,
    ) -> Result<Arc<Model>, ModelError> {
        let context = Arc::new(ModelContext::new(params.runtime_config, instance_registry)?);
        let root = ComponentInstance::new_root(
            params.root_environment,
            context.clone(),
            Arc::downgrade(&params.top_instance),
            params.root_component_url,
        )
        .await;
        let model =
            Arc::new(Model { root: root.clone(), context, top_instance: params.top_instance });
        model.top_instance.init(root).await;
        Ok(model)
    }

    /// Returns a reference to the instance at the top of the tree (component manager's own
    /// instance).
    pub fn top_instance(&self) -> &Arc<ComponentManagerInstance> {
        &self.top_instance
    }

    /// Returns a reference to the root component instance.
    pub fn root(&self) -> &Arc<ComponentInstance> {
        &self.root
    }

    pub fn context(&self) -> &ModelContext {
        &self.context
    }

    pub fn component_id_index(&self) -> &component_id_index::Index {
        self.context.component_id_index()
    }

    /// Discovers the root component, providing it with `dict_for_root`.
    pub async fn discover_root_component(self: &Arc<Model>, input_for_root: ComponentInput) {
        ActionSet::register(self.root.clone(), DiscoverAction::new(input_for_root))
            .await
            .expect("failed to discover root component");
    }

    /// Starts root, starting the component tree.
    ///
    /// If `discover_root_component` has already been called, then `input_for_root` is unused.
    pub async fn start(self: &Arc<Model>, input_for_root: ComponentInput) {
        // Normally the Discovered event is dispatched when an instance is added as a child, but
        // since the root isn't anyone's child we need to dispatch it here.
        self.discover_root_component(input_for_root).await;

        // In debug mode, we don't start the component root. It must be started manually from
        // the lifecycle controller.
        if self.context.runtime_config().debug {
            warn!(
                "In debug mode, the root component will not be started. Use the LifecycleController
                protocol to start the root component."
            );
        } else {
            if let Err(e) = self.root.ensure_started(&StartReason::Root).await {
                // Starting root may take a long time as it will be resolving and starting
                // eager children. If graceful shutdown is initiated, that will cause those
                // children to fail to resolve or fail to start, and for `start` to fail.
                //
                // If we fail to start the root, but the root is being shutdown, or already
                // shutdown, that's ok. The system is tearing down, so it doesn't matter any more
                // if we never got everything started that we wanted to.
                let action_set = self.root.lock_actions().await;
                if !action_set.contains(ActionKey::Shutdown).await {
                    if !self.root.lock_state().await.is_shut_down() {
                        panic!(
                            "failed to start root component {}: {:?}",
                            self.root.component_url, e
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use {
        crate::model::{
            actions::{ActionSet, ShutdownAction, ShutdownType},
            hooks::{Event, EventType, Hook, HooksRegistration},
            model::Model,
            structured_dict::ComponentInput,
            testing::test_helpers::{TestEnvironmentBuilder, TestModelResult},
        },
        async_trait::async_trait,
        cm_rust_testing::*,
        errors::ModelError,
        moniker::Moniker,
        std::sync::{Arc, Weak},
    };

    #[fuchsia::test]
    async fn already_shut_down_when_start_fails() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("bad-scheme").url("bad-scheme://sdf").eager())
                .build(),
        )];

        let TestModelResult { model, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let _ =
            ActionSet::register(model.root.clone(), ShutdownAction::new(ShutdownType::Instance))
                .await
                .unwrap();

        model.start(ComponentInput::default()).await;
    }

    #[fuchsia::test]
    async fn shutting_down_when_start_fails() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("bad-scheme").url("bad-scheme://sdf").eager())
                .build(),
        )];

        let TestModelResult { model, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        // Wait for the child to be discovered -- this happens in the middle of starting
        // the root component (eagerly starting children), then register the shutdown
        // action. This means the root will already be scheduled for shutdown after
        // start inevitably fails.
        struct RegisterShutdown {
            model: Arc<Model>,
        }

        #[async_trait]
        impl Hook for RegisterShutdown {
            async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
                if event.target_moniker != "bad-scheme".parse::<Moniker>().unwrap().into() {
                    return Ok(());
                }
                let _ =
                    self.model.root().lock_actions().await.register_inner(
                        &self.model.root,
                        ShutdownAction::new(ShutdownType::Instance),
                    );
                Ok(())
            }
        }

        let hook = Arc::new(RegisterShutdown { model: model.clone() });
        model
            .root()
            .hooks
            .install(vec![HooksRegistration::new(
                "shutdown_root_on_child_discover",
                vec![EventType::Discovered],
                Arc::downgrade(&hook) as Weak<dyn Hook>,
            )])
            .await;

        model.start(ComponentInput::default()).await;
    }

    #[should_panic]
    #[fuchsia::test]
    async fn not_shutting_down_when_start_fails() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("bad-scheme").url("bad-scheme://sdf").eager())
                .build(),
        )];

        let TestModelResult { model, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        model.start(ComponentInput::default()).await;
    }
}
