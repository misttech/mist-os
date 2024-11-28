// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::component_instance::{ComponentInstanceForAnalyzer, TopInstanceForAnalyzer};
use crate::component_model::{BuildAnalyzerModelError, Child};
use cm_config::RuntimeConfig;
use cm_rust::{EnvironmentDecl, RegistrationSource, ResolverRegistration};
use fidl_fuchsia_component_internal as component_internal;
use routing::component_instance::{ExtendedInstanceInterface, WeakExtendedInstanceInterface};
use routing::environment::{DebugRegistry, EnvironmentExtends, RunnerRegistry};
use routing::error::ComponentInstanceError;
use std::collections::HashMap;
use std::sync::Arc;

// Constants used to set up the built-in environment.
pub static BOOT_RESOLVER_NAME: &str = "boot_resolver";
pub static BOOT_SCHEME: &str = "fuchsia-boot";

pub static PKG_RESOLVER_NAME: &str = "package_resolver";
pub static PKG_SCHEME: &str = "fuchsia-pkg";

static REALM_BUILDER_RESOLVER_NAME: &str = "realm_builder_resolver";
static REALM_BUILDER_SCHEME: &str = "realm-builder";

/// A collection of resolver registrations, keyed by target URL scheme.
#[derive(Clone, Debug, Default)]
pub struct ResolverRegistry {
    resolvers: HashMap<String, ResolverRegistration>,
}

impl ResolverRegistry {
    pub fn new() -> ResolverRegistry {
        Self { resolvers: HashMap::default() }
    }

    pub fn from_decl(decl: &[ResolverRegistration]) -> Result<Self, BuildAnalyzerModelError> {
        let mut registry = Self::new();
        for resolver in decl.iter() {
            if let Some(reg) = registry.register(resolver) {
                return Err(BuildAnalyzerModelError::DuplicateResolverScheme(reg.scheme));
            }
        }
        Ok(registry)
    }

    pub fn register(&mut self, resolver: &ResolverRegistration) -> Option<ResolverRegistration> {
        self.resolvers.insert(resolver.scheme.clone(), resolver.clone())
    }

    pub fn get_resolver(&self, scheme: &str) -> Option<&ResolverRegistration> {
        self.resolvers.get(scheme)
    }
}

/// A representation of a v2 component instance's environment and its relationship to the
/// parent realm's environment.
#[derive(Debug)]
pub struct EnvironmentForAnalyzer {
    env: routing::environment::Environment<ComponentInstanceForAnalyzer>,
    /// The resolvers available in this environment.
    resolver_registry: ResolverRegistry,
}

impl EnvironmentForAnalyzer {
    pub fn env(&self) -> &routing::environment::Environment<ComponentInstanceForAnalyzer> {
        &self.env
    }
    // TODO(https://fxbug.dev/42140194): This parallel implementation of component manager's builtin environment
    // setup will do for now, but is fragile and should be replaced soon. In particular, it doesn't provide a
    // way to register builtin runners or resolvers that appear in the `builtin_capabilities` field of the
    // RuntimeConfig but are not one of these hard-coded built-ins.
    pub fn new_root(
        runner_registry: RunnerRegistry,
        runtime_config: &Arc<RuntimeConfig>,
        top_instance: &Arc<TopInstanceForAnalyzer>,
    ) -> Arc<Self> {
        let mut resolver_registry = ResolverRegistry::default();

        // Register the boot resolver, if any
        match runtime_config.builtin_boot_resolver {
            component_internal::BuiltinBootResolver::Boot => {
                assert!(
                    resolver_registry
                        .register(&ResolverRegistration {
                            resolver: BOOT_RESOLVER_NAME.parse().unwrap(),
                            source: RegistrationSource::Self_,
                            scheme: BOOT_SCHEME.to_string(),
                        })
                        .is_none(),
                    "found duplicate resolver for boot scheme"
                );
            }
            component_internal::BuiltinBootResolver::None => {}
        };

        // Register the RealmBuilder resolver and runner, if any
        match runtime_config.realm_builder_resolver_and_runner {
            component_internal::RealmBuilderResolverAndRunner::Namespace => {
                assert!(
                    resolver_registry
                        .register(&ResolverRegistration {
                            resolver: REALM_BUILDER_RESOLVER_NAME.parse().unwrap(),
                            source: RegistrationSource::Self_,
                            scheme: REALM_BUILDER_SCHEME.to_string(),
                        })
                        .is_none(),
                    "found duplicate resolver for realm builder scheme"
                );
            }
            component_internal::RealmBuilderResolverAndRunner::None => {}
        };

        Arc::new(Self {
            env: routing::environment::Environment::new(
                None,
                WeakExtendedInstanceInterface::from(&ExtendedInstanceInterface::AboveRoot(
                    Arc::clone(top_instance),
                )),
                EnvironmentExtends::None,
                runner_registry,
                DebugRegistry::default(),
            ),
            resolver_registry,
        })
    }

    pub fn new_for_child(
        parent: &Arc<ComponentInstanceForAnalyzer>,
        child: &Child,
    ) -> Result<Arc<Self>, BuildAnalyzerModelError> {
        match child.environment.as_ref() {
            Some(child_env_name) => {
                let env_decl = parent
                    .decl_for_testing()
                    .environments
                    .iter()
                    .find(|&env| &env.name == child_env_name)
                    .ok_or_else(|| {
                        BuildAnalyzerModelError::EnvironmentNotFound(
                            child_env_name.to_string(),
                            child.child_moniker.name().to_string(),
                            parent.moniker().to_string(),
                        )
                    })?;
                Self::new_from_decl(parent, env_decl)
            }
            None => Ok(Self::new_inheriting(parent)),
        }
    }

    fn new_inheriting(parent: &Arc<ComponentInstanceForAnalyzer>) -> Arc<Self> {
        Arc::new(Self {
            env: routing::environment::Environment::new(
                None,
                WeakExtendedInstanceInterface::from(&ExtendedInstanceInterface::Component(
                    Arc::clone(parent),
                )),
                EnvironmentExtends::Realm,
                RunnerRegistry::default(),
                DebugRegistry::default(),
            ),
            resolver_registry: ResolverRegistry::default(),
        })
    }

    fn new_from_decl(
        parent: &Arc<ComponentInstanceForAnalyzer>,
        env_decl: &EnvironmentDecl,
    ) -> Result<Arc<Self>, BuildAnalyzerModelError> {
        Ok(Arc::new(Self {
            env: routing::environment::Environment::new(
                Some(env_decl.name.clone()),
                WeakExtendedInstanceInterface::from(&ExtendedInstanceInterface::Component(
                    Arc::clone(parent),
                )),
                env_decl.extends.into(),
                RunnerRegistry::from_decl(&env_decl.runners),
                env_decl.debug_capabilities.clone().into(),
            ),
            resolver_registry: ResolverRegistry::from_decl(&env_decl.resolvers)?,
        }))
    }

    /// Returns the resolver registered for `scheme` and the component that created the environment the
    /// resolver was registered to. Returns `None` if there was no match.
    pub fn get_registered_resolver(
        &self,
        scheme: &str,
    ) -> Result<
        Option<(ExtendedInstanceInterface<ComponentInstanceForAnalyzer>, ResolverRegistration)>,
        ComponentInstanceError,
    > {
        let parent = self.env.parent().upgrade()?;
        match self.resolver_registry().get_resolver(scheme) {
            Some(reg) => Ok(Some((parent, reg.clone()))),
            None => match self.env.extends() {
                EnvironmentExtends::Realm => match parent {
                    ExtendedInstanceInterface::Component(parent) => {
                        parent.environment.get_registered_resolver(scheme)
                    }
                    ExtendedInstanceInterface::AboveRoot(_) => {
                        unreachable!("root env can't extend")
                    }
                },
                EnvironmentExtends::None => {
                    return Ok(None);
                }
            },
        }
    }

    fn resolver_registry(&self) -> &ResolverRegistry {
        &self.resolver_registry
    }
}
