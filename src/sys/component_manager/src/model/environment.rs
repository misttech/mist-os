// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::manager::ComponentManagerInstance;
use crate::model::component::{ComponentInstance, WeakExtendedInstance};
use crate::model::resolver::{Resolver, ResolverRegistry};
use ::routing::environment::{DebugRegistry, EnvironmentExtends, RunnerRegistry};
use cm_rust::EnvironmentDecl;
use fidl_fuchsia_component_decl as fdecl;
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use std::sync::Weak;

/// A realm's environment, populated from a component's [`EnvironmentDecl`].
/// An environment defines intrinsic behaviors of a component's realm. Components
/// can define an environment, but do not interact with it directly.
///
/// [`EnvironmentDecl`]: fidl_fuchsia_sys2::EnvironmentDecl
#[derive(Debug)]
pub struct Environment {
    env: routing::environment::Environment<ComponentInstance>,
    /// The resolvers in this environment, mapped to URL schemes.
    resolver_registry: ResolverRegistry,
    /// The deadline for runners to respond to `ComponentController.Stop` calls.
    stop_timeout: Duration,
}

pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(5);

impl Environment {
    /// Creates a new empty environment parented to component manager.
    #[cfg(test)]
    pub fn empty() -> Environment {
        Environment {
            env: routing::environment::Environment::new(
                None,
                WeakExtendedInstance::AboveRoot(Weak::new()),
                EnvironmentExtends::None,
                RunnerRegistry::default(),
                DebugRegistry::default(),
            ),
            resolver_registry: ResolverRegistry::new(),
            stop_timeout: DEFAULT_STOP_TIMEOUT,
        }
    }

    /// Creates a new root environment with a resolver registry, parented to component manager.
    pub fn new_root(
        top_instance: &Arc<ComponentManagerInstance>,
        runner_registry: RunnerRegistry,
        resolver_registry: ResolverRegistry,
        debug_registry: DebugRegistry,
    ) -> Environment {
        Environment {
            env: routing::environment::Environment::new(
                None,
                WeakExtendedInstance::AboveRoot(Arc::downgrade(top_instance)),
                EnvironmentExtends::None,
                runner_registry,
                debug_registry,
            ),
            resolver_registry,
            stop_timeout: DEFAULT_STOP_TIMEOUT,
        }
    }

    /// Creates an environment from `env_decl`, using `parent` as the parent realm.
    pub fn from_decl(parent: &Arc<ComponentInstance>, env_decl: &EnvironmentDecl) -> Environment {
        Environment {
            env: routing::environment::Environment::new(
                Some(env_decl.name.clone()),
                WeakExtendedInstance::Component(parent.into()),
                env_decl.extends.into(),
                RunnerRegistry::from_decl(&env_decl.runners),
                env_decl.debug_capabilities.clone().into(),
            ),
            resolver_registry: ResolverRegistry::new(),
            stop_timeout: match env_decl.stop_timeout_ms {
                Some(timeout) => Duration::from_millis(timeout.into()),
                None => match env_decl.extends {
                    fdecl::EnvironmentExtends::Realm => parent.environment.stop_timeout(),
                    fdecl::EnvironmentExtends::None => {
                        panic!("EnvironmentDecl is missing stop_timeout");
                    }
                },
            },
        }
    }

    /// Creates a new environment with `parent` as the parent.
    pub fn new_inheriting(parent: &Arc<ComponentInstance>) -> Environment {
        Environment {
            env: routing::environment::Environment::new(
                None,
                WeakExtendedInstance::Component(parent.into()),
                EnvironmentExtends::Realm,
                RunnerRegistry::default(),
                DebugRegistry::default(),
            ),
            resolver_registry: ResolverRegistry::new(),
            stop_timeout: parent.environment.stop_timeout(),
        }
    }

    pub fn stop_timeout(&self) -> Duration {
        self.stop_timeout
    }

    pub fn environment(&self) -> &routing::environment::Environment<ComponentInstance> {
        &self.env
    }

    pub fn drain_resolvers<'a>(
        &'a mut self,
    ) -> impl Iterator<Item = (String, Arc<dyn Resolver + Send + Sync + 'static>)> + 'a {
        self.resolver_registry.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin_environment::RootComponentInputBuilder;
    use crate::model::component::{ExtendedInstance, StartReason};
    use crate::model::context::ModelContext;
    use crate::model::model::{Model, ModelParams};
    use crate::model::testing::mocks::MockResolver;
    use crate::model::token::InstanceRegistry;
    use ::routing::bedrock::structured_dict::ComponentInput;
    use ::routing::environment::DebugRegistration;
    use ::routing::policy::PolicyError;
    use assert_matches::assert_matches;
    use cm_config::{
        AllowlistEntryBuilder, CapabilityAllowlistSource, DebugCapabilityAllowlistEntry,
        DebugCapabilityKey, RuntimeConfig, SecurityPolicy,
    };
    use cm_rust::{RegistrationSource, RunnerRegistration};
    use cm_rust_testing::{
        ChildBuilder, CollectionBuilder, ComponentDeclBuilder, EnvironmentBuilder,
    };
    use cm_types::Name;
    use cm_util::TaskGroup;
    use errors::{ActionError, ModelError, ResolveActionError};
    use fidl_fuchsia_component as fcomponent;
    use maplit::hashmap;
    use moniker::Moniker;
    use std::collections::{HashMap, HashSet};

    #[fuchsia::test]
    async fn test_from_decl() {
        let component = ComponentInstance::new_root(
            ComponentInput::default(),
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "test:///root".parse().unwrap(),
        )
        .await;
        let environment = Environment::from_decl(
            &component,
            &EnvironmentBuilder::new()
                .name("env")
                .extends(fdecl::EnvironmentExtends::None)
                .stop_timeout(1234)
                .build(),
        );
        assert_matches!(environment.env.parent(), WeakExtendedInstance::Component(_));

        let environment = Environment::from_decl(
            &component,
            &EnvironmentBuilder::new()
                .name("env")
                .extends(fdecl::EnvironmentExtends::Realm)
                .build(),
        );
        assert_matches!(environment.env.parent(), WeakExtendedInstance::Component(_));

        let environment = Environment::from_decl(
            &component,
            &EnvironmentBuilder::new()
                .name("env")
                .extends(fdecl::EnvironmentExtends::None)
                .stop_timeout(1234)
                .debug(cm_rust::DebugRegistration::Protocol(cm_rust::DebugProtocolRegistration {
                    source_name: "source_name".parse().unwrap(),
                    target_name: "target_name".parse().unwrap(),
                    source: RegistrationSource::Parent,
                }))
                .build(),
        );
        let expected_debug_capability: HashMap<Name, DebugRegistration> = hashmap! {
            "target_name".parse().unwrap() =>
            DebugRegistration {
                source_name: "source_name".parse().unwrap(),
                source: RegistrationSource::Parent,
            }
        };
        assert_eq!(environment.env.debug_registry().debug_capabilities, expected_debug_capability);
    }

    #[fuchsia::test]
    async fn test_debug_policy_error() {
        for runtime_config in vec![
            make_debug_allowlisting_config("source_name", "env_a", Moniker::root()),
            make_debug_allowlisting_config("target_name", "env_b", Moniker::root()),
            make_debug_allowlisting_config("target_name", "env_a", "a".try_into().unwrap()),
        ] {
            let resolver = MockResolver::new();
            resolver.add_component(
                "root",
                ComponentDeclBuilder::new_empty_component()
                    .child(ChildBuilder::new().name("a").environment("env_a"))
                    .environment(EnvironmentBuilder::new().name("env_a").debug(
                        cm_rust::DebugRegistration::Protocol(cm_rust::DebugProtocolRegistration {
                            source_name: "source_name".parse().unwrap(),
                            target_name: "target_name".parse().unwrap(),
                            source: RegistrationSource::Parent,
                        }),
                    ))
                    .build(),
            );
            resolver.add_component(
                "a",
                ComponentDeclBuilder::new_empty_component()
                    .environment(EnvironmentBuilder::new().name("env_b"))
                    .build(),
            );

            let task_group = TaskGroup::new();
            let mut root_input_builder =
                RootComponentInputBuilder::new(task_group.as_weak(), &runtime_config);
            root_input_builder.add_resolver("test".to_string(), Arc::new(resolver));

            let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
            let model = Model::new(
                ModelParams {
                    runtime_config,
                    root_component_url: "test:///root".parse().unwrap(),
                    root_environment: Environment::new_root(
                        &top_instance,
                        RunnerRegistry::new(HashMap::new()),
                        ResolverRegistry::new(),
                        DebugRegistry::default(),
                    ),
                    top_instance,
                    instance_registry: InstanceRegistry::new(),
                    scope_factory: None,
                },
                root_input_builder.build(),
            )
            .await
            .unwrap();
            assert_matches!(
                model.root().resolve().await,
                Err(ActionError::ResolveError {
                    err: ResolveActionError::Policy(
                        PolicyError::DebugCapabilityUseDisallowed { .. }
                    )
                })
            );
        }
    }

    // Each component declares an environment for their child that inherits from the component's
    // environment. The leaf component should be able to access the resolvers of the root.
    #[fuchsia::test]
    async fn test_inherit_root() -> Result<(), ModelError> {
        let runner_reg = RunnerRegistration {
            source: RegistrationSource::Parent,
            source_name: "test".parse().unwrap(),
            target_name: "test".parse().unwrap(),
        };
        let runners: HashMap<Name, RunnerRegistration> = hashmap! {
            "test".parse().unwrap() => runner_reg.clone()
        };

        let debug_reg = DebugRegistration {
            source_name: "source_name".parse().unwrap(),
            source: RegistrationSource::Self_,
        };

        let debug_capabilities: HashMap<Name, DebugRegistration> = hashmap! {
            "target_name".parse().unwrap() => debug_reg.clone()
        };
        let debug_registry = DebugRegistry { debug_capabilities };

        let resolver = MockResolver::new();
        resolver.add_component(
            "root",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("a").environment("env_a"))
                .environment(
                    EnvironmentBuilder::new()
                        .name("env_a")
                        .extends(fdecl::EnvironmentExtends::Realm),
                )
                .build(),
        );
        resolver.add_component(
            "a",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("b").environment("env_b"))
                .environment(
                    EnvironmentBuilder::new()
                        .name("env_b")
                        .extends(fdecl::EnvironmentExtends::Realm),
                )
                .build(),
        );
        resolver.add_component("b", ComponentDeclBuilder::new_empty_component().build());

        let task_group = TaskGroup::new();
        let config = Arc::new(RuntimeConfig::default());
        let mut root_input_builder = RootComponentInputBuilder::new(task_group.as_weak(), &config);
        root_input_builder.add_resolver("test".to_string(), Arc::new(resolver));

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let model = Model::new(
            ModelParams {
                runtime_config: config,
                root_component_url: "test:///root".parse().unwrap(),
                root_environment: Environment::new_root(
                    &top_instance,
                    RunnerRegistry::new(runners),
                    ResolverRegistry::new(),
                    debug_registry,
                ),
                top_instance,
                instance_registry: InstanceRegistry::new(),
                scope_factory: None,
            },
            root_input_builder.build(),
        )
        .await
        .unwrap();
        let component = model
            .root()
            .start_instance(&vec!["a", "b"].try_into().unwrap(), &StartReason::Eager)
            .await?;
        assert_eq!(component.component_url, "test:///b");

        let registered_runner =
            component.environment.env.get_registered_runner(&"test".parse().unwrap()).unwrap();
        assert_matches!(registered_runner, Some((ExtendedInstance::AboveRoot(_), r)) if r == runner_reg);
        assert_matches!(
            component.environment.env.get_registered_runner(&"foo".parse().unwrap()),
            Ok(None)
        );

        let debug_capability = component
            .environment
            .env
            .get_debug_capability(&"target_name".parse().unwrap())
            .unwrap();
        assert_matches!(debug_capability, Some((ExtendedInstance::AboveRoot(_), None, d)) if d == debug_reg);
        assert_matches!(
            component.environment.env.get_debug_capability(&"foo".parse().unwrap()),
            Ok(None)
        );

        Ok(())
    }

    // A component declares an environment that inherits from realm, and the realm's environment
    // added something that should be available in the component's realm.
    #[fuchsia::test]
    async fn test_inherit_parent() -> Result<(), ModelError> {
        let runner_reg = RunnerRegistration {
            source: RegistrationSource::Parent,
            source_name: "test".parse().unwrap(),
            target_name: "test".parse().unwrap(),
        };
        let runners: HashMap<Name, RunnerRegistration> = hashmap! {
            "test".parse().unwrap() => runner_reg.clone()
        };

        let debug_reg = DebugRegistration {
            source_name: "source_name".parse().unwrap(),
            source: RegistrationSource::Parent,
        };

        let resolver = MockResolver::new();
        resolver.add_component(
            "root",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("a").environment("env_a"))
                .environment(
                    EnvironmentBuilder::new()
                        .name("env_a")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .runner(runner_reg.clone())
                        .debug(cm_rust::DebugRegistration::Protocol(
                            cm_rust::DebugProtocolRegistration {
                                source_name: "source_name".parse().unwrap(),
                                target_name: "target_name".parse().unwrap(),
                                source: RegistrationSource::Parent,
                            },
                        )),
                )
                .build(),
        );
        resolver.add_component(
            "a",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("b").environment("env_b"))
                .environment(
                    EnvironmentBuilder::new()
                        .name("env_b")
                        .extends(fdecl::EnvironmentExtends::Realm),
                )
                .build(),
        );
        resolver.add_component("b", ComponentDeclBuilder::new_empty_component().build());

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let runtime_config =
            make_debug_allowlisting_config("target_name", "env_a", Moniker::root());
        let task_group = TaskGroup::new();
        let mut root_input_builder =
            RootComponentInputBuilder::new(task_group.as_weak(), &runtime_config);
        root_input_builder.add_resolver("test".to_string(), Arc::new(resolver));

        let model = Model::new(
            ModelParams {
                runtime_config,
                root_component_url: "test:///root".parse().unwrap(),
                root_environment: Environment::new_root(
                    &top_instance,
                    RunnerRegistry::new(runners),
                    ResolverRegistry::new(),
                    DebugRegistry::default(),
                ),
                top_instance,
                instance_registry: InstanceRegistry::new(),
                scope_factory: None,
            },
            root_input_builder.build(),
        )
        .await?;
        let component = model
            .root()
            .start_instance(&vec!["a", "b"].try_into().unwrap(), &StartReason::Eager)
            .await?;
        assert_eq!(component.component_url, "test:///b");

        let registered_runner =
            component.environment.env.get_registered_runner(&"test".parse().unwrap()).unwrap();
        assert_matches!(registered_runner, Some((ExtendedInstance::Component(c), r))
            if r == runner_reg && c.moniker == Moniker::root());
        assert_matches!(
            component.environment.env.get_registered_runner(&"foo".parse().unwrap()),
            Ok(None)
        );

        let debug_capability = component
            .environment
            .env
            .get_debug_capability(&"target_name".parse().unwrap())
            .unwrap();
        assert_matches!(debug_capability, Some((ExtendedInstance::Component(c), Some(_), d))
            if d == debug_reg && c.moniker == Moniker::root());
        assert_matches!(
            component.environment.env.get_debug_capability(&"foo".parse().unwrap()),
            Ok(None)
        );

        Ok(())
    }

    fn make_debug_allowlisting_config(
        name: &str,
        env_name: &str,
        env_moniker: Moniker,
    ) -> Arc<RuntimeConfig> {
        let mut allowlist = HashSet::new();
        allowlist.insert(DebugCapabilityAllowlistEntry::new(
            AllowlistEntryBuilder::build_exact_from_moniker(&env_moniker),
        ));
        let mut debug_capability_policy = HashMap::new();
        debug_capability_policy.insert(
            DebugCapabilityKey {
                name: name.parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: cm_rust::CapabilityTypeName::Protocol,
                env_name: env_name.parse().unwrap(),
            },
            allowlist,
        );
        let security_policy =
            Arc::new(SecurityPolicy { debug_capability_policy, ..Default::default() });
        Arc::new(RuntimeConfig { security_policy, ..Default::default() })
    }

    // A component in a collection declares an environment that inherits from realm, and the
    // realm's environment added something that should be available in the component's realm.
    #[fuchsia::test]
    async fn test_inherit_in_collection() -> Result<(), ModelError> {
        let runner_reg = RunnerRegistration {
            source: RegistrationSource::Parent,
            source_name: "test".parse().unwrap(),
            target_name: "test".parse().unwrap(),
        };
        let runners: HashMap<Name, RunnerRegistration> = hashmap! {
            "test".parse().unwrap() => runner_reg.clone()
        };

        let debug_reg = DebugRegistration {
            source_name: "source_name".parse().unwrap(),
            source: RegistrationSource::Parent,
        };

        let resolver = MockResolver::new();
        resolver.add_component(
            "root",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("a").environment("env_a"))
                .environment(
                    EnvironmentBuilder::new()
                        .name("env_a")
                        .extends(fdecl::EnvironmentExtends::Realm)
                        .runner(RunnerRegistration {
                            source: RegistrationSource::Parent,
                            source_name: "test".parse().unwrap(),
                            target_name: "test".parse().unwrap(),
                        })
                        .debug(cm_rust::DebugRegistration::Protocol(
                            cm_rust::DebugProtocolRegistration {
                                source_name: "source_name".parse().unwrap(),
                                target_name: "target_name".parse().unwrap(),
                                source: RegistrationSource::Parent,
                            },
                        )),
                )
                .build(),
        );
        resolver.add_component(
            "a",
            ComponentDeclBuilder::new_empty_component()
                .collection(CollectionBuilder::new().name("coll").environment("env_b"))
                .environment(
                    EnvironmentBuilder::new()
                        .name("env_b")
                        .extends(fdecl::EnvironmentExtends::Realm),
                )
                .build(),
        );
        resolver.add_component("b", ComponentDeclBuilder::new_empty_component().build());

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));

        let runtime_config =
            make_debug_allowlisting_config("target_name", "env_a", Moniker::root());

        let task_group = TaskGroup::new();
        let mut root_input_builder =
            RootComponentInputBuilder::new(task_group.as_weak(), &runtime_config);
        root_input_builder.add_resolver("test".to_string(), Arc::new(resolver));

        let model = Model::new(
            ModelParams {
                runtime_config,
                root_component_url: "test:///root".parse().unwrap(),
                root_environment: Environment::new_root(
                    &top_instance,
                    RunnerRegistry::new(runners),
                    ResolverRegistry::new(),
                    DebugRegistry::default(),
                ),
                top_instance,
                instance_registry: InstanceRegistry::new(),
                scope_factory: None,
            },
            root_input_builder.build(),
        )
        .await?;
        // Add instance to collection.
        {
            let parent = model
                .root()
                .start_instance(&vec!["a"].try_into().unwrap(), &StartReason::Eager)
                .await?;
            let child_decl = ChildBuilder::new().name("b").build();
            parent
                .add_dynamic_child(
                    "coll".into(),
                    &child_decl,
                    fcomponent::CreateChildArgs::default(),
                )
                .await
                .expect("failed to add child");
        }
        let component = model
            .root()
            .start_instance(&vec!["a", "coll:b"].try_into().unwrap(), &StartReason::Eager)
            .await?;
        assert_eq!(component.component_url, "test:///b");

        let registered_runner =
            component.environment.env.get_registered_runner(&"test".parse().unwrap()).unwrap();
        assert_matches!(registered_runner, Some((ExtendedInstance::Component(c), r))
            if r == runner_reg && c.moniker == Moniker::root());
        assert_matches!(
            component.environment.env.get_registered_runner(&"foo".parse().unwrap()),
            Ok(None)
        );

        let debug_capability = component
            .environment
            .env
            .get_debug_capability(&"target_name".parse().unwrap())
            .unwrap();
        assert_matches!(debug_capability, Some((ExtendedInstance::Component(c), Some(n), d))
            if d == debug_reg && n == "env_a" && c.moniker == Moniker::root());
        assert_matches!(
            component.environment.env.get_debug_capability(&"foo".parse().unwrap()),
            Ok(None)
        );

        Ok(())
    }

    // One of the components does not declare or specify an environment for the leaf child. The
    // leaf child component should still be able to access the resolvers of the root, as an
    // implicit inheriting environment is assumed.
    #[fuchsia::test]
    async fn test_auto_inheritance() -> Result<(), ModelError> {
        let runner_reg = RunnerRegistration {
            source: RegistrationSource::Parent,
            source_name: "test-src".parse().unwrap(),
            target_name: "test".parse().unwrap(),
        };
        let runners: HashMap<Name, RunnerRegistration> = hashmap! {
            "test".parse().unwrap() => runner_reg.clone()
        };

        let debug_reg = DebugRegistration {
            source_name: "source_name".parse().unwrap(),
            source: RegistrationSource::Parent,
        };

        let debug_capabilities: HashMap<Name, DebugRegistration> = hashmap! {
            "target_name".parse().unwrap() => debug_reg.clone()
        };
        let debug_registry = DebugRegistry { debug_capabilities };

        let resolver = MockResolver::new();
        resolver.add_component(
            "root",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("a").environment("env_a"))
                .environment(
                    EnvironmentBuilder::new()
                        .name("env_a")
                        .extends(fdecl::EnvironmentExtends::Realm),
                )
                .build(),
        );
        resolver.add_component(
            "a",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("b"))
                .build(),
        );
        resolver.add_component("b", ComponentDeclBuilder::new_empty_component().build());

        let task_group = TaskGroup::new();
        let config = Arc::new(RuntimeConfig::default());
        let mut root_input_builder = RootComponentInputBuilder::new(task_group.as_weak(), &config);
        root_input_builder.add_resolver("test".to_string(), Arc::new(resolver));

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let model = Model::new(
            ModelParams {
                runtime_config: config,
                root_component_url: "test:///root".parse().unwrap(),
                root_environment: Environment::new_root(
                    &top_instance,
                    RunnerRegistry::new(runners),
                    ResolverRegistry::new(),
                    debug_registry,
                ),
                top_instance,
                instance_registry: InstanceRegistry::new(),
                scope_factory: None,
            },
            root_input_builder.build(),
        )
        .await
        .unwrap();

        let component = model
            .root()
            .start_instance(&vec!["a", "b"].try_into().unwrap(), &StartReason::Eager)
            .await?;
        assert_eq!(component.component_url, "test:///b");

        let registered_runner =
            component.environment.env.get_registered_runner(&"test".parse().unwrap()).unwrap();
        assert_matches!(registered_runner, Some((ExtendedInstance::AboveRoot(_), r)) if r == runner_reg);
        assert_matches!(
            component.environment.env.get_registered_runner(&"foo".parse().unwrap()),
            Ok(None)
        );

        let debug_capability = component
            .environment
            .env
            .get_debug_capability(&"target_name".parse().unwrap())
            .unwrap();
        assert_matches!(debug_capability, Some((ExtendedInstance::AboveRoot(_), None, d)) if d == debug_reg);
        assert_matches!(
            component.environment.env.get_debug_capability(&"foo".parse().unwrap()),
            Ok(None)
        );

        Ok(())
    }

    // One of the components declares an environment that does not inherit from the realm. This
    // means that any child components of this component cannot be resolved.
    #[fuchsia::test]
    async fn test_resolver_no_inheritance() -> Result<(), ModelError> {
        let resolver = MockResolver::new();
        resolver.add_component(
            "root",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("a").environment("env_a"))
                .environment(
                    EnvironmentBuilder::new()
                        .name("env_a")
                        .extends(fdecl::EnvironmentExtends::Realm),
                )
                .build(),
        );
        resolver.add_component(
            "a",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("b").environment("env_b"))
                .environment(
                    EnvironmentBuilder::new()
                        .name("env_b")
                        .extends(fdecl::EnvironmentExtends::None)
                        .stop_timeout(1234),
                )
                .build(),
        );
        resolver.add_component("b", ComponentDeclBuilder::new_empty_component().build());
        let registry = {
            let mut registry = ResolverRegistry::new();
            registry.register("test".to_string(), Arc::new(resolver));
            registry
        };
        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let model = Model::new(
            ModelParams {
                runtime_config: Arc::new(RuntimeConfig::default()),
                root_component_url: "test:///root".parse().unwrap(),
                root_environment: Environment::new_root(
                    &top_instance,
                    RunnerRegistry::default(),
                    registry,
                    DebugRegistry::default(),
                ),
                top_instance,
                instance_registry: InstanceRegistry::new(),
                scope_factory: None,
            },
            ComponentInput::default(),
        )
        .await?;
        assert_matches!(
            model
                .root()
                .start_instance(&vec!["a", "b"].try_into().unwrap(), &StartReason::Eager)
                .await,
            Err(ModelError::ActionError { err: ActionError::ResolveError { .. } })
        );
        Ok(())
    }
}
