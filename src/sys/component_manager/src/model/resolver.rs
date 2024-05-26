// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::{ComponentInstance, WeakComponentInstance},
        routing::{open_capability, RouteRequest},
    },
    ::routing::{
        component_instance::ComponentInstanceInterface,
        resolving::{ComponentAddress, ResolvedComponent, ResolverError},
    },
    async_trait::async_trait,
    cm_rust::{ConfigValueSource, FidlIntoNative, ResolverRegistration},
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_resolution as fresolution,
    fidl_fuchsia_mem as fmem,
    std::{collections::HashMap, sync::Arc},
    tracing::error,
};

/// Resolves a component URL to its content.
#[async_trait]
pub trait Resolver: std::fmt::Debug {
    /// Resolves a component URL to its content. This function takes in the
    /// `component_address` (from an absolute or relative URL), and the `target`
    /// component that is trying to be resolved.
    async fn resolve(
        &self,
        component_address: &ComponentAddress,
    ) -> Result<ResolvedComponent, ResolverError>;
}

/// Resolves a component URL using a resolver selected based on the URL's scheme.
#[derive(Debug, Default)]
pub struct ResolverRegistry {
    resolvers: HashMap<String, Box<dyn Resolver + Send + Sync + 'static>>,
}

impl ResolverRegistry {
    pub fn new() -> ResolverRegistry {
        Default::default()
    }

    pub fn register(
        &mut self,
        scheme: String,
        resolver: Box<dyn Resolver + Send + Sync + 'static>,
    ) {
        // ComponentDecl validation checks that there aren't any duplicate schemes.
        assert!(
            self.resolvers.insert(scheme, resolver).is_none(),
            "Found duplicate scheme in ComponentDecl"
        );
    }

    /// Creates and populates a `ResolverRegistry` with `RemoteResolvers` that
    /// have been registered with an environment.
    pub fn from_decl(decl: &[ResolverRegistration], parent: &Arc<ComponentInstance>) -> Self {
        let mut registry = ResolverRegistry::new();
        for resolver in decl {
            registry.register(
                resolver.scheme.clone().into(),
                Box::new(RemoteResolver::new(resolver.clone(), parent.as_weak())),
            );
        }
        registry
    }
}

#[async_trait]
impl Resolver for ResolverRegistry {
    async fn resolve(
        &self,
        component_address: &ComponentAddress,
    ) -> Result<ResolvedComponent, ResolverError> {
        if let Some(resolver) = self.resolvers.get(component_address.scheme()) {
            resolver.resolve(component_address).await
        } else {
            Err(ResolverError::SchemeNotRegistered)
        }
    }
}

/// A resolver whose implementation lives in an external component. The source
/// of the resolver is determined through capability routing.
#[derive(Debug)]
pub struct RemoteResolver {
    registration: ResolverRegistration,
    component: WeakComponentInstance,
}

impl RemoteResolver {
    pub fn new(registration: ResolverRegistration, component: WeakComponentInstance) -> Self {
        RemoteResolver { registration, component }
    }
}

// TODO(61288): Implement some sort of caching of the routed capability. Multiple
// component URL resolutions should be possible on a single channel.
#[async_trait]
impl Resolver for RemoteResolver {
    async fn resolve(
        &self,
        component_address: &ComponentAddress,
    ) -> Result<ResolvedComponent, ResolverError> {
        let component = self.component.upgrade().map_err(ResolverError::routing_error)?;
        let proxy: fresolution::ResolverProxy =
            open_capability(&RouteRequest::Resolver(self.registration.clone()), &component)
                .await
                .map_err(ResolverError::routing_error)?;
        let (component_url, some_context) = component_address.to_url_and_context();
        let component = if component_address.is_relative_path() {
            let context = some_context.ok_or_else(|| {
                error!(url=%component_url, "calling resolve_with_context() with absolute");
                ResolverError::RelativeUrlMissingContext(component_url.to_string())
            })?;
            proxy
                .resolve_with_context(component_url, &context.into())
                .await
                .map_err(ResolverError::fidl_error)??
        } else {
            proxy.resolve(component_url).await.map_err(ResolverError::fidl_error)??
        };
        let decl_buffer: fmem::Data = component.decl.ok_or(ResolverError::RemoteInvalidData)?;
        let decl = read_and_validate_manifest(&decl_buffer)?;
        let config_values = match &decl.config {
            Some(config) => match config.value_source {
                ConfigValueSource::PackagePath(_) => Some(read_and_validate_config_values(
                    &component.config_values.ok_or(ResolverError::RemoteInvalidData)?,
                )?),
                ConfigValueSource::Capabilities(_) => None,
            },
            None => None,
        };
        let resolved_url = component.url.ok_or(ResolverError::RemoteInvalidData)?;
        let context_to_resolve_children = component.resolution_context.map(Into::into);
        let abi_revision = component.abi_revision.map(Into::into);
        Ok(ResolvedComponent {
            resolved_url,
            context_to_resolve_children,
            decl,
            package: component.package.map(TryInto::try_into).transpose()?,
            config_values,
            abi_revision,
        })
    }
}

/// Given a ref-counted resolver, returns a boxed resolver that delegates to the ref-counted
/// resolver.
pub fn box_arc_resolver(
    arc: &Arc<impl Resolver + Send + Sync + 'static>,
) -> Box<dyn Resolver + Send + Sync + 'static> {
    Box::new(InternalResolver(arc.clone()))
}

#[derive(Debug)]
struct InternalResolver(Arc<dyn Resolver + Send + Sync + 'static>);

#[async_trait]
impl Resolver for InternalResolver {
    async fn resolve(
        &self,
        component_address: &ComponentAddress,
    ) -> Result<ResolvedComponent, ResolverError> {
        self.0.resolve(component_address).await
    }
}

pub fn read_and_validate_manifest(
    data: &fmem::Data,
) -> Result<cm_rust::ComponentDecl, ResolverError> {
    let bytes = mem_util::bytes_from_data(data).map_err(ResolverError::manifest_invalid)?;
    read_and_validate_manifest_bytes(&bytes)
}

pub fn read_and_validate_manifest_bytes(
    bytes: &[u8],
) -> Result<cm_rust::ComponentDecl, ResolverError> {
    let component_decl: fdecl::Component =
        fidl::unpersist(bytes).map_err(ResolverError::manifest_invalid)?;
    cm_fidl_validator::validate(&component_decl).map_err(ResolverError::manifest_invalid)?;
    Ok(component_decl.fidl_into_native())
}

pub fn read_and_validate_config_values(
    data: &fmem::Data,
) -> Result<cm_rust::ConfigValuesData, ResolverError> {
    let bytes = mem_util::bytes_from_data(&data).map_err(ResolverError::config_values_invalid)?;
    let values = fidl::unpersist(&bytes).map_err(ResolverError::fidl_error)?;
    cm_fidl_validator::validate_values_data(&values)
        .map_err(|e| ResolverError::config_values_invalid(e))?;
    Ok(values.fidl_into_native())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::{
            actions::DiscoverAction,
            component::{manager::ComponentManagerInstance, WeakExtendedInstance},
            context::ModelContext,
            environment::Environment,
            structured_dict::ComponentInput,
        },
        anyhow::{format_err, Error},
        assert_matches::assert_matches,
        async_trait::async_trait,
        cm_rust::NativeIntoFidl,
        cm_rust_testing::new_decl_from_json,
        fidl_fuchsia_component_decl as fdecl,
        hooks::Hooks,
        lazy_static::lazy_static,
        moniker::Moniker,
        moniker::MonikerBase,
        routing::environment::{DebugRegistry, RunnerRegistry},
        routing::resolving::ComponentResolutionContext,
        serde_json::json,
        std::sync::{Mutex, Weak},
    };

    #[derive(Debug)]
    struct MockOkResolver {
        pub expected_url: String,
        pub resolved_url: String,
    }

    #[async_trait]
    impl Resolver for MockOkResolver {
        async fn resolve(
            &self,
            component_address: &ComponentAddress,
        ) -> Result<ResolvedComponent, ResolverError> {
            assert_eq!(&self.expected_url, component_address.url());
            Ok(ResolvedComponent {
                resolved_url: self.resolved_url.clone(),
                // MockOkResolver only resolves one component, so it does not
                // need to provide a context for resolving children.
                context_to_resolve_children: None,
                decl: cm_rust::ComponentDecl::default(),
                package: None,
                config_values: None,
                abi_revision: Some(
                    version_history::HISTORY.get_example_supported_version_for_tests().abi_revision,
                ),
            })
        }
    }

    struct MockErrorResolver {
        pub expected_url: String,
        pub error: Box<dyn Fn(&str) -> ResolverError + Send + Sync + 'static>,
    }

    impl core::fmt::Debug for MockErrorResolver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockErrorResolver").finish()
        }
    }

    #[async_trait]
    impl Resolver for MockErrorResolver {
        async fn resolve(
            &self,
            component_address: &ComponentAddress,
        ) -> Result<ResolvedComponent, ResolverError> {
            assert_eq!(self.expected_url, component_address.url());
            Err((self.error)(component_address.url()))
        }
    }

    #[derive(Debug, Clone)]
    struct ResolveState {
        pub expected_url: String,
        pub resolved_url: String,
        pub expected_context: Option<ComponentResolutionContext>,
        pub context_to_resolve_children: Option<ComponentResolutionContext>,
    }

    impl ResolveState {
        fn new(
            url: &str,
            expected_context: Option<ComponentResolutionContext>,
            context_to_resolve_children: Option<ComponentResolutionContext>,
        ) -> Self {
            Self {
                expected_url: url.to_string(),
                resolved_url: url.to_string(),
                expected_context,
                context_to_resolve_children,
            }
        }
    }

    #[derive(Debug)]
    struct MockMultipleOkResolver {
        pub resolve_states: Arc<Mutex<Vec<ResolveState>>>,
    }

    impl MockMultipleOkResolver {
        fn new(resolve_states: Vec<ResolveState>) -> Self {
            Self { resolve_states: Arc::new(Mutex::new(resolve_states)) }
        }
    }

    #[async_trait]
    impl Resolver for MockMultipleOkResolver {
        async fn resolve(
            &self,
            component_address: &ComponentAddress,
        ) -> Result<ResolvedComponent, ResolverError> {
            let ResolveState {
                expected_url,
                resolved_url,
                expected_context,
                context_to_resolve_children,
            } = self.resolve_states.lock().unwrap().remove(0);
            let (component_url, some_context) = component_address.to_url_and_context();
            assert_eq!(expected_url, component_url);
            assert_eq!(expected_context.as_ref(), some_context, "resolving {}", component_url);
            Ok(ResolvedComponent {
                resolved_url,
                context_to_resolve_children,

                // We don't actually need to return a valid component here as these unit tests only
                // cover the process of going from relative -> full URL.
                decl: cm_rust::ComponentDecl::default(),
                package: None,
                config_values: None,
                abi_revision: Some(
                    version_history::HISTORY.get_example_supported_version_for_tests().abi_revision,
                ),
            })
        }
    }

    async fn new_root_discovered_component(
        environment: Environment,
        context: Arc<ModelContext>,
        component_manager_instance: Weak<ComponentManagerInstance>,
        component_url: &str,
    ) -> Arc<ComponentInstance> {
        let component = ComponentInstance::new_root(
            environment,
            context,
            component_manager_instance,
            component_url.parse().unwrap(),
        )
        .await;
        // We don't care about waiting for the discover action to complete, just that it's started.
        let _ = component
            .actions()
            .register_no_wait(DiscoverAction::new(ComponentInput::default()))
            .await;
        component
    }

    async fn new_discovered_component(
        environment: Arc<Environment>,
        moniker: Moniker,
        component_url: &str,
        startup: fdecl::StartupMode,
        on_terminate: fdecl::OnTerminate,
        config_parent_overrides: Option<Vec<cm_rust::ConfigOverride>>,
        context: Arc<ModelContext>,
        parent: WeakExtendedInstance,
        hooks: Arc<Hooks>,
        persistent_storage: bool,
    ) -> Arc<ComponentInstance> {
        let component = ComponentInstance::new(
            environment,
            moniker,
            0,
            component_url.parse().unwrap(),
            startup,
            on_terminate,
            config_parent_overrides,
            context,
            parent,
            hooks,
            persistent_storage,
        )
        .await;
        // We don't care about waiting for the discover action to complete, just that it's started.
        let _ = component
            .actions()
            .register_no_wait(DiscoverAction::new(ComponentInput::default()))
            .await;
        component
    }

    fn address_from_absolute_url(url: &str) -> ComponentAddress {
        ComponentAddress::from_absolute_url(&url.parse().unwrap()).unwrap()
    }

    async fn address_from(
        url: &str,
        instance: &Arc<ComponentInstance>,
    ) -> Result<ComponentAddress, ResolverError> {
        ComponentAddress::from(&url.parse().unwrap(), instance).await
    }

    #[fuchsia_async::run_until_stalled(test)]
    async fn register_and_resolve() {
        let mut registry = ResolverRegistry::new();
        registry.register(
            "foo".to_string(),
            Box::new(MockOkResolver {
                expected_url: "foo://url".to_string(),
                resolved_url: "foo://resolved".to_string(),
            }),
        );
        registry.register(
            "bar".to_string(),
            Box::new(MockErrorResolver {
                expected_url: "bar://url".to_string(),
                error: Box::new(|_| {
                    ResolverError::manifest_not_found(format_err!("not available"))
                }),
            }),
        );

        let root = new_root_discovered_component(
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-boot:///#meta/root.cm",
        )
        .await;

        // Resolve known scheme that returns success.
        let component = registry.resolve(&address_from_absolute_url("foo://url")).await.unwrap();
        assert_eq!("foo://resolved", component.resolved_url);

        // Resolve a different scheme that produces an error.
        let expected_res: Result<ResolvedComponent, ResolverError> =
            Err(ResolverError::manifest_not_found(format_err!("not available")));
        assert_eq!(
            format!("{:?}", expected_res),
            format!("{:?}", registry.resolve(&address_from_absolute_url("bar://url")).await)
        );

        // Resolve an unknown scheme
        let expected_res: Result<ResolvedComponent, ResolverError> =
            Err(ResolverError::SchemeNotRegistered);
        assert_eq!(
            format!("{:?}", expected_res),
            format!("{:?}", registry.resolve(&address_from_absolute_url("unknown://url")).await),
        );

        // Resolve a possible relative path (e.g., subpackage) URL lacking a
        // resolvable parent causes a SchemeNotRegistered.
        assert_matches!(
            address_from("xxx#meta/comp.cm", &root).await,
            Err(ResolverError::NoParentContext(_))
        );
    }

    #[fuchsia::test]
    #[should_panic(expected = "Found duplicate scheme in ComponentDecl")]
    fn test_duplicate_registration() {
        let mut registry = ResolverRegistry::new();
        let resolver_a =
            MockOkResolver { expected_url: "".to_string(), resolved_url: "".to_string() };
        let resolver_b =
            MockOkResolver { expected_url: "".to_string(), resolved_url: "".to_string() };
        registry.register("fuchsia-pkg".to_string(), Box::new(resolver_a));
        registry.register("fuchsia-pkg".to_string(), Box::new(resolver_b));
    }

    #[fuchsia::test]
    fn test_multiple_scheme_registration() {
        let mut registry = ResolverRegistry::new();
        let resolver_a =
            MockOkResolver { expected_url: "".to_string(), resolved_url: "".to_string() };
        let resolver_b =
            MockOkResolver { expected_url: "".to_string(), resolved_url: "".to_string() };
        registry.register("fuchsia-pkg".to_string(), Box::new(resolver_a));
        registry.register("fuchsia-boot".to_string(), Box::new(resolver_b));
    }

    lazy_static! {
        static ref COMPONENT_DECL: cm_rust::ComponentDecl = new_decl_from_json(json!(
        {
            "include": [ "syslog/client.shard.cml" ],
            "program": {
                "runner": "elf",
                "binary": "bin/example",
            },
            "children": [
                {
                    "name": "logger",
                    "url": "fuchsia-pkg://fuchsia.com/logger/stable#meta/logger.cm",
                    "environment": "#env_one",
                },
            ],
            "collections": [
                {
                    "name": "modular",
                    "durability": "transient",
                },
            ],
            "capabilities": [
                {
                    "protocol": "fuchsia.logger.Log2",
                    "path": "/svc/fuchsia.logger.Log2",
                },
            ],
            "use": [
                {
                    "protocol": "fuchsia.fonts.LegacyProvider",
                },
            ],
            "environments": [
                {
                    "name": "env_one",
                    "extends": "none",
                    "__stop_timeout_ms": 1337,
                },
            ],
            "facets": {
                "author": "Fuchsia",
            }}))
        .expect("failed to construct manifest");
    }

    #[fuchsia::test]
    fn test_read_and_validate_manifest() {
        let manifest = fmem::Data::Bytes(
            fidl::persist(&COMPONENT_DECL.clone().native_into_fidl())
                .expect("failed to encode manifest"),
        );
        let actual = read_and_validate_manifest(&manifest).expect("failed to decode manifest");
        assert_eq!(actual, COMPONENT_DECL.clone());
    }

    #[fuchsia::test]
    async fn test_read_and_validate_config_values() {
        let fidl_config_values = fdecl::ConfigValuesData {
            values: Some(vec![
                fdecl::ConfigValueSpec {
                    value: Some(fdecl::ConfigValue::Single(fdecl::ConfigSingleValue::Bool(false))),
                    ..Default::default()
                },
                fdecl::ConfigValueSpec {
                    value: Some(fdecl::ConfigValue::Single(fdecl::ConfigSingleValue::Uint8(5))),
                    ..Default::default()
                },
                fdecl::ConfigValueSpec {
                    value: Some(fdecl::ConfigValue::Single(fdecl::ConfigSingleValue::String(
                        "hello!".to_string(),
                    ))),
                    ..Default::default()
                },
                fdecl::ConfigValueSpec {
                    value: Some(fdecl::ConfigValue::Vector(fdecl::ConfigVectorValue::BoolVector(
                        vec![true, false],
                    ))),
                    ..Default::default()
                },
                fdecl::ConfigValueSpec {
                    value: Some(fdecl::ConfigValue::Vector(
                        fdecl::ConfigVectorValue::StringVector(vec![
                            "hello!".to_string(),
                            "world!".to_string(),
                        ]),
                    )),
                    ..Default::default()
                },
            ]),
            checksum: Some(fdecl::ConfigChecksum::Sha256([0; 32])),
            ..Default::default()
        };
        let config_values = cm_rust::ConfigValuesData {
            values: vec![
                cm_rust::ConfigValueSpec {
                    value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Bool(false)),
                },
                cm_rust::ConfigValueSpec {
                    value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Uint8(5)),
                },
                cm_rust::ConfigValueSpec {
                    value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::String(
                        "hello!".to_string(),
                    )),
                },
                cm_rust::ConfigValueSpec {
                    value: cm_rust::ConfigValue::Vector(cm_rust::ConfigVectorValue::BoolVector(
                        vec![true, false],
                    )),
                },
                cm_rust::ConfigValueSpec {
                    value: cm_rust::ConfigValue::Vector(cm_rust::ConfigVectorValue::StringVector(
                        vec!["hello!".to_string(), "world!".to_string()],
                    )),
                },
            ],
            checksum: cm_rust::ConfigChecksum::Sha256([0; 32]),
        };
        let data = fmem::Data::Bytes(
            fidl::persist(&fidl_config_values).expect("failed to encode config values"),
        );
        let actual =
            read_and_validate_config_values(&data).expect("failed to decode config values");
        assert_eq!(actual, config_values);
    }

    #[fuchsia::test]
    async fn test_from_absolute_component_url_with_component_instance() -> Result<(), Error> {
        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            ResolverRegistry::new(),
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-pkg://fuchsia.com/package#meta/comp.cm",
        )
        .await;

        let abs = address_from("fuchsia-pkg://fuchsia.com/package#meta/comp.cm", &root).await?;
        assert_matches!(abs, ComponentAddress::Absolute { .. });
        assert_eq!(abs.scheme(), "fuchsia-pkg");
        assert_eq!(abs.path(), "/package");
        assert_eq!(abs.resource(), Some("meta/comp.cm"));
        Ok(())
    }

    #[fuchsia::test]
    async fn test_from_relative_path_component_url_with_component_instance() -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "fuchsia-pkg://fuchsia.com/package#meta/comp.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "subpackage#meta/subcomp.cm",
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("subpackage_context".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "fuchsia-pkg".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-pkg://fuchsia.com/package#meta/comp.cm",
        )
        .await;
        let child = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "subpackage#meta/subcomp.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let relpath = address_from("subpackage#meta/subcomp.cm", &child).await?;
        assert_matches!(relpath, ComponentAddress::RelativePath { .. });
        assert_eq!(relpath.path(), "subpackage");
        assert_eq!(relpath.resource(), Some("meta/subcomp.cm"));
        assert_eq!(
            relpath.context(),
            &ComponentResolutionContext::new("package_context".as_bytes().to_vec())
        );

        Ok(())
    }

    #[fuchsia::test]
    async fn test_from_relative_path_component_url_with_fuchsia_boot_component_instance(
    ) -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "fuchsia-boot:///package#meta/comp.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "subpackage#meta/subcomp.cm",
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("subpackage_context".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "fuchsia-boot".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-boot:///package#meta/comp.cm",
        )
        .await;
        let child = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "subpackage#meta/subcomp.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let relpath = address_from("subpackage#meta/subcomp.cm", &child).await?;
        assert_matches!(relpath, ComponentAddress::RelativePath { .. });
        assert_eq!(relpath.path(), "subpackage");
        assert_eq!(relpath.resource(), Some("meta/subcomp.cm"));
        assert_eq!(
            relpath.context(),
            &ComponentResolutionContext::new("package_context".as_bytes().to_vec())
        );
        Ok(())
    }

    #[fuchsia::test]
    async fn test_from_relative_path_component_url_with_cast_component_instance(
    ) -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "cast:00000000/package#meta/comp.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "subpackage#meta/subcomp.cm",
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("subpackage_context".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "cast".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "cast:00000000/package#meta/comp.cm",
        )
        .await;
        let child = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "subpackage#meta/subcomp.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let relpath = address_from("subpackage#meta/subcomp.cm", &child).await?;
        assert_matches!(relpath, ComponentAddress::RelativePath { .. });
        assert_eq!(relpath.path(), "subpackage");
        assert_eq!(relpath.resource(), Some("meta/subcomp.cm"));
        assert_eq!(
            relpath.context(),
            &ComponentResolutionContext::new("package_context".as_bytes().to_vec())
        );
        Ok(())
    }

    #[fuchsia::test]
    async fn relative_to_fuchsia_pkg() -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "fuchsia-pkg://fuchsia.com/my-package#meta/my-child.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "fuchsia-pkg".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
        )
        .await;

        let child = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "#meta/my-child.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let resolved = child
            .environment
            .resolve(&ComponentAddress::from(&child.component_url, &child).await?)
            .await?;
        let expected = expected_urls_and_contexts.as_slice().last().unwrap();
        assert_eq!(&resolved.resolved_url, &expected.resolved_url);
        assert_eq!(&resolved.context_to_resolve_children, &expected.context_to_resolve_children);

        Ok(())
    }

    #[fuchsia::test]
    async fn two_relative_to_fuchsia_pkg() -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "fuchsia-pkg://fuchsia.com/my-package#meta/my-child.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "fuchsia-pkg://fuchsia.com/my-package#meta/my-child2.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "fuchsia-pkg".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
        )
        .await;

        let child_one = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "#meta/my-child.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let child_two = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "#meta/my-child2.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&child_one)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let resolved = child_two
            .environment
            .resolve(&ComponentAddress::from(&child_two.component_url, &child_two).await?)
            .await?;
        let expected = expected_urls_and_contexts.as_slice().last().unwrap();
        assert_eq!(&resolved.resolved_url, &expected.resolved_url);
        assert_eq!(&resolved.context_to_resolve_children, &expected.context_to_resolve_children);
        Ok(())
    }

    #[fuchsia::test]
    async fn relative_to_fuchsia_boot() -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "fuchsia-boot:///#meta/my-root.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "fuchsia-boot:///#meta/my-child.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "fuchsia-boot".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-boot:///#meta/my-root.cm",
        )
        .await;

        let child = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "#meta/my-child.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let resolved = child
            .environment
            .resolve(&ComponentAddress::from(&child.component_url, &child).await?)
            .await?;
        let expected = expected_urls_and_contexts.as_slice().last().unwrap();
        assert_eq!(&resolved.resolved_url, &expected.resolved_url);
        assert_eq!(&resolved.context_to_resolve_children, &expected.context_to_resolve_children);
        Ok(())
    }

    #[fuchsia::test]
    async fn relative_to_cast() -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "cast:00000000#meta/my-root.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "cast:00000000#meta/my-child.cm",
                None,
                Some(ComponentResolutionContext::new("package_context".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "cast".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "cast:00000000#meta/my-root.cm",
        )
        .await;

        let child = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "#meta/my-child.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let resolved = child
            .environment
            .resolve(&ComponentAddress::from(&child.component_url, &child).await?)
            .await?;
        let expected = expected_urls_and_contexts.as_slice().last().unwrap();
        assert_eq!(&resolved.resolved_url, &expected.resolved_url);
        assert_eq!(&resolved.context_to_resolve_children, &expected.context_to_resolve_children);
        Ok(())
    }

    #[fuchsia::test]
    async fn resolve_above_root_error() -> Result<(), Error> {
        let resolver = ResolverRegistry::new();

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "#meta/my-root.cm",
        )
        .await;

        let child = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "#meta/my-child.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let result = ComponentAddress::from(&child.component_url, &child).await;
        assert_matches!(result, Err(ResolverError::Internal(..)));
        Ok(())
    }

    #[fuchsia::test]
    async fn relative_resource_and_path_to_fuchsia_pkg() -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
                None,
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "my-subpackage#meta/my-child.cm",
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("my-subpackage...".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "my-subpackage#meta/my-child2.cm",
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("my-subpackage...".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "fuchsia-pkg".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
        )
        .await;

        let child_one = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "my-subpackage#meta/my-child.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let child_two = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child/child2")?,
            "#meta/my-child2.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&child_one)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let resolved = child_two
            .environment
            .resolve(&ComponentAddress::from(&child_two.component_url, &child_two).await?)
            .await?;
        let expected = expected_urls_and_contexts.as_slice().last().unwrap();
        assert_eq!(&resolved.resolved_url, &expected.resolved_url);
        assert_eq!(&resolved.context_to_resolve_children, &expected.context_to_resolve_children);
        Ok(())
    }

    #[fuchsia::test]
    async fn two_relative_resources_and_path_to_fuchsia_pkg() -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
                None,
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "my-subpackage#meta/my-child.cm",
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("my-subpackage...".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "my-subpackage#meta/my-child2.cm",
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("my-subpackage...".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "my-subpackage#meta/my-child3.cm",
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("my-subpackage...".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "fuchsia-pkg".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );
        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
        )
        .await;

        let child_one = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child")?,
            "my-subpackage#meta/my-child.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let child_two = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child/child2")?,
            "#meta/my-child2.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&child_one)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let child_three = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/child/child2/child3")?,
            "#meta/my-child3.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&child_two)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let resolved = child_three
            .environment
            .resolve(&ComponentAddress::from(&child_three.component_url, &child_three).await?)
            .await?;
        let expected = expected_urls_and_contexts.as_slice().last().unwrap();
        assert_eq!(&resolved.resolved_url, &expected.resolved_url);
        assert_eq!(&resolved.context_to_resolve_children, &expected.context_to_resolve_children);
        Ok(())
    }

    #[fuchsia::test]
    async fn relative_resources_and_paths_to_realm_builder() -> Result<(), Error> {
        let expected_urls_and_contexts = vec![
            ResolveState::new(
                "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
                None,
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "my-subpackage1#meta/sub1.cm",
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("my-subpackage1...".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "my-subpackage1#meta/sub1-child.cm",
                Some(ComponentResolutionContext::new("fuchsia.com...".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("my-subpackage1...".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "my-subpackage2#meta/sub2.cm",
                Some(ComponentResolutionContext::new("my-subpackage1...".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("my-subpackage2...".as_bytes().to_vec())),
            ),
            ResolveState::new(
                "my-subpackage2#meta/sub2-child.cm",
                Some(ComponentResolutionContext::new("my-subpackage1...".as_bytes().to_vec())),
                Some(ComponentResolutionContext::new("my-subpackage2...".as_bytes().to_vec())),
            ),
        ];
        let mut resolver = ResolverRegistry::new();

        resolver.register(
            "fuchsia-pkg".to_string(),
            Box::new(MockMultipleOkResolver::new(expected_urls_and_contexts.clone())),
        );
        resolver.register(
            "realm-builder".to_string(),
            Box::new(MockOkResolver {
                expected_url: "realm-builder://0/my-realm".to_string(),
                resolved_url: "realm-builder://0/my-realm".to_string(),
            }),
        );

        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        let environment = Environment::new_root(
            &top_instance,
            RunnerRegistry::default(),
            resolver,
            DebugRegistry::default(),
        );

        let root = new_root_discovered_component(
            environment,
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "fuchsia-pkg://fuchsia.com/my-package#meta/my-root.cm",
        )
        .await;

        let realm = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/realm/child")?,
            "realm-builder://0/my-realm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&root)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let child_one = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/realm/child")?,
            "my-subpackage1#meta/sub1.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&realm)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let child_two = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/realm/child/child2")?,
            "#meta/sub1-child.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&child_one)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let child_three = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/realm/child/child2/child3")?,
            "my-subpackage2#meta/sub2.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&child_two)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let child_four = new_discovered_component(
            root.environment.clone(),
            Moniker::parse_str("root/realm/child/child2/child3/child4")?,
            "#meta/sub2-child.cm",
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::from(&child_three)),
            Arc::new(Hooks::new()),
            false,
        )
        .await;

        let resolved = child_four
            .environment
            .resolve(&ComponentAddress::from(&child_four.component_url, &child_four).await?)
            .await?;
        let expected = expected_urls_and_contexts.as_slice().last().unwrap();
        assert_eq!(&resolved.resolved_url, &expected.resolved_url);
        assert_eq!(&resolved.context_to_resolve_children, &expected.context_to_resolve_children);
        Ok(())
    }
}
