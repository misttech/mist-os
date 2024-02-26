// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    cm_rust::{ComponentDecl, FidlIntoNative},
    cm_types::{Name, Path, RelativePath},
    cml,
    derivative::Derivative,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio,
    std::path::PathBuf,
};

/// Name of the test runner.
///
/// Several functions assume the existence of a runner with this name.
pub const TEST_RUNNER_NAME: &str = "test_runner";

/// Deserialize `object` into a cml::Document and then translate the result
/// to ComponentDecl.
pub fn new_decl_from_json(object: serde_json::Value) -> Result<ComponentDecl, Error> {
    let doc = serde_json::from_value(object).context("failed to deserialize manifest")?;
    let cm =
        cml::compile(&doc, cml::CompileOptions::default()).context("failed to compile manifest")?;
    Ok(cm.fidl_into_native())
}

/// Builder for constructing a ComponentDecl.
#[derive(Debug, Clone)]
pub struct ComponentDeclBuilder {
    result: ComponentDecl,
}

impl ComponentDeclBuilder {
    /// An empty ComponentDeclBuilder, with no program.
    pub fn new_empty_component() -> Self {
        ComponentDeclBuilder { result: Default::default() }
    }

    /// A ComponentDeclBuilder prefilled with a program and using a runner named "test_runner",
    /// which we assume is offered to us.
    pub fn new() -> Self {
        Self::new_empty_component().add_program(TEST_RUNNER_NAME)
    }

    /// Add a child element.
    pub fn child(mut self, decl: impl Into<cm_rust::ChildDecl>) -> Self {
        self.result.children.push(decl.into());
        self
    }

    /// Add a child with default properties.
    pub fn child_default(self, name: &str) -> Self {
        self.child(ChildBuilder::new().name(name))
    }

    // Add a collection element.
    pub fn collection(mut self, decl: impl Into<cm_rust::CollectionDecl>) -> Self {
        self.result.collections.push(decl.into());
        self
    }

    /// Add a collection with default properties.
    pub fn collection_default(self, name: &str) -> Self {
        self.collection(CollectionBuilder::new().name(name))
    }

    /// Add a "program" clause, using the given runner.
    pub fn add_program(mut self, runner: &str) -> Self {
        assert!(self.result.program.is_none(), "tried to add program twice");
        self.result.program = Some(cm_rust::ProgramDecl {
            runner: Some(runner.parse().unwrap()),
            info: fdata::Dictionary { entries: Some(vec![]), ..Default::default() },
        });
        self
    }

    /// Add a custom offer.
    pub fn offer(mut self, offer: cm_rust::OfferDecl) -> Self {
        self.result.offers.push(offer);
        self
    }

    /// Add a custom expose.
    pub fn expose(mut self, expose: cm_rust::ExposeDecl) -> Self {
        self.result.exposes.push(expose);
        self
    }

    /// Add a custom use decl.
    pub fn use_(mut self, use_: cm_rust::UseDecl) -> Self {
        self.result.uses.push(use_);
        self
    }

    // Add a use decl for fuchsia.component.Realm.
    pub fn use_realm(mut self) -> Self {
        let use_ = cm_rust::UseDecl::Protocol(cm_rust::UseProtocolDecl {
            dependency_type: cm_rust::DependencyType::Strong,
            source: cm_rust::UseSource::Framework,
            source_name: "fuchsia.component.Realm".parse().unwrap(),
            source_dictionary: None,
            target_path: "/svc/fuchsia.component.Realm".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });
        self.result.uses.push(use_);
        self
    }

    /// Add a capability declaration.
    pub fn capability(mut self, capability: impl Into<cm_rust::CapabilityDecl>) -> Self {
        self.result.capabilities.push(capability.into());
        self
    }

    /// Add a default protocol declaration.
    pub fn protocol_default(self, name: &str) -> Self {
        self.capability(ProtocolBuilder::new().name(name).build())
    }

    /// Add a default dictionary declaration.
    pub fn dictionary_default(self, name: &str) -> Self {
        self.capability(DictionaryBuilder::new().name(name).build())
    }

    /// Add a default runner declaration.
    pub fn runner_default(self, name: &str) -> Self {
        self.capability(RunnerBuilder::new().name(name).build())
    }

    /// Add a default resolver declaration.
    pub fn resolver_default(self, name: &str) -> Self {
        self.capability(ResolverBuilder::new().name(name).build())
    }

    /// Add a default service declaration.
    pub fn service_default(self, name: &str) -> Self {
        self.capability(ServiceBuilder::new().name(name).build())
    }

    /// Add an environment declaration.
    pub fn environment(mut self, environment: impl Into<cm_rust::EnvironmentDecl>) -> Self {
        self.result.environments.push(environment.into());
        self
    }

    /// Add a config declaration.
    pub fn config(mut self, config: cm_rust::ConfigDecl) -> Self {
        self.result.config = Some(config);
        self
    }

    /// Generate the final ComponentDecl.
    pub fn build(self) -> ComponentDecl {
        self.result
    }
}

/// A convenience builder for constructing ChildDecls.
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct ChildBuilder {
    name: Option<String>,
    url: Option<String>,
    #[derivative(Default(value = "fdecl::StartupMode::Lazy"))]
    startup: fdecl::StartupMode,
    on_terminate: Option<fdecl::OnTerminate>,
    environment: Option<String>,
}

impl ChildBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Defaults url to `"test:///{name}"`.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.into());
        if self.url.is_none() {
            self.url = Some(format!("test:///{name}"));
        }
        self
    }

    pub fn url(mut self, url: &str) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn startup(mut self, startup: fdecl::StartupMode) -> Self {
        self.startup = startup;
        self
    }

    pub fn eager(self) -> Self {
        self.startup(fdecl::StartupMode::Eager)
    }

    pub fn on_terminate(mut self, on_terminate: fdecl::OnTerminate) -> Self {
        self.on_terminate = Some(on_terminate);
        self
    }

    pub fn environment(mut self, environment: &str) -> Self {
        self.environment = Some(environment.into());
        self
    }

    pub fn build(self) -> cm_rust::ChildDecl {
        cm_rust::ChildDecl {
            name: self.name.expect("name not set"),
            url: self.url.expect("url not set"),
            startup: self.startup,
            on_terminate: self.on_terminate,
            environment: self.environment,
            config_overrides: None,
        }
    }
}

impl From<ChildBuilder> for cm_rust::ChildDecl {
    fn from(builder: ChildBuilder) -> Self {
        builder.build()
    }
}

/// A convenience builder for constructing CollectionDecls.
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct CollectionBuilder {
    name: Option<Name>,
    #[derivative(Default(value = "fdecl::Durability::Transient"))]
    durability: fdecl::Durability,
    environment: Option<String>,
    #[derivative(Default(value = "cm_types::AllowedOffers::StaticOnly"))]
    allowed_offers: cm_types::AllowedOffers,
    allow_long_names: bool,
    persistent_storage: Option<bool>,
}

impl CollectionBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        self
    }

    pub fn durability(mut self, durability: fdecl::Durability) -> Self {
        self.durability = durability;
        self
    }

    pub fn environment(mut self, environment: &str) -> Self {
        self.environment = Some(environment.into());
        self
    }

    pub fn allowed_offers(mut self, allowed_offers: cm_types::AllowedOffers) -> Self {
        self.allowed_offers = allowed_offers;
        self
    }

    pub fn allow_long_names(mut self) -> Self {
        self.allow_long_names = true;
        self
    }

    pub fn persistent_storage(mut self, persistent_storage: bool) -> Self {
        self.persistent_storage = Some(persistent_storage);
        self
    }

    pub fn build(self) -> cm_rust::CollectionDecl {
        cm_rust::CollectionDecl {
            name: self.name.expect("name not set"),
            durability: self.durability,
            environment: self.environment,
            allowed_offers: self.allowed_offers,
            allow_long_names: self.allow_long_names,
            persistent_storage: self.persistent_storage,
        }
    }
}

impl From<CollectionBuilder> for cm_rust::CollectionDecl {
    fn from(builder: CollectionBuilder) -> Self {
        builder.build()
    }
}

/// A convenience builder for constructing EnvironmentDecls.
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct EnvironmentBuilder {
    name: Option<String>,
    #[derivative(Default(value = "fdecl::EnvironmentExtends::Realm"))]
    extends: fdecl::EnvironmentExtends,
    runners: Vec<cm_rust::RunnerRegistration>,
    resolvers: Vec<cm_rust::ResolverRegistration>,
    debug_capabilities: Vec<cm_rust::DebugRegistration>,
    stop_timeout_ms: Option<u32>,
}

impl EnvironmentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn extends(mut self, extends: fdecl::EnvironmentExtends) -> Self {
        self.extends = extends;
        self
    }

    pub fn runner(mut self, runner: cm_rust::RunnerRegistration) -> Self {
        self.runners.push(runner);
        self
    }

    pub fn resolver(mut self, resolver: cm_rust::ResolverRegistration) -> Self {
        self.resolvers.push(resolver);
        self
    }

    pub fn debug(mut self, debug: cm_rust::DebugRegistration) -> Self {
        self.debug_capabilities.push(debug);
        self
    }

    pub fn stop_timeout(mut self, timeout_ms: u32) -> Self {
        self.stop_timeout_ms = Some(timeout_ms);
        self
    }

    pub fn build(self) -> cm_rust::EnvironmentDecl {
        cm_rust::EnvironmentDecl {
            name: self.name.expect("name not set"),
            extends: self.extends,
            runners: self.runners,
            resolvers: self.resolvers,
            debug_capabilities: self.debug_capabilities,
            stop_timeout_ms: self.stop_timeout_ms,
        }
    }
}

impl From<EnvironmentBuilder> for cm_rust::EnvironmentDecl {
    fn from(builder: EnvironmentBuilder) -> Self {
        builder.build()
    }
}

// A convenience builder for constructing ProtocolDecls.
#[derive(Debug, Default)]
pub struct ProtocolBuilder {
    name: Option<Name>,
    path: Option<Path>,
}

impl ProtocolBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Defaults `source_path` to `/svc/{name}`.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        if self.path.is_none() {
            self.path = Some(format!("/svc/{name}").parse().unwrap());
        }
        self
    }

    pub fn path(mut self, path: &str) -> Self {
        self.path = Some(path.parse().unwrap());
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        cm_rust::CapabilityDecl::Protocol(cm_rust::ProtocolDecl {
            name: self.name.expect("name not set"),
            source_path: Some(self.path.expect("path not set")),
        })
    }
}

impl From<ProtocolBuilder> for cm_rust::CapabilityDecl {
    fn from(builder: ProtocolBuilder) -> Self {
        builder.build()
    }
}

// A convenience builder for constructing [DictionaryDecl]s.
#[derive(Debug, Default)]
pub struct DictionaryBuilder {
    name: Option<Name>,
    source: Option<cm_rust::DictionarySource>,
    source_dictionary: Option<RelativePath>,
}

impl DictionaryBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        self
    }

    pub fn source_dictionary(
        mut self,
        source: cm_rust::DictionarySource,
        source_dictionary: &str,
    ) -> Self {
        self.source = Some(source);
        self.source_dictionary = Some(source_dictionary.parse().unwrap());
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        cm_rust::CapabilityDecl::Dictionary(cm_rust::DictionaryDecl {
            name: self.name.expect("name not set"),
            source: self.source,
            source_dictionary: self.source_dictionary,
        })
    }
}

impl From<DictionaryBuilder> for cm_rust::CapabilityDecl {
    fn from(builder: DictionaryBuilder) -> Self {
        builder.build()
    }
}

// A convenience builder for constructing ServiceDecls.
#[derive(Debug, Default)]
pub struct ServiceBuilder {
    name: Option<Name>,
    path: Option<Path>,
}

impl ServiceBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Defaults `source_path` to `/svc/{name}`.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        if self.path.is_none() {
            self.path = Some(format!("/svc/{name}").parse().unwrap());
        }
        self
    }

    pub fn path(mut self, path: &str) -> Self {
        self.path = Some(path.parse().unwrap());
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        cm_rust::CapabilityDecl::Service(cm_rust::ServiceDecl {
            name: self.name.expect("name not set"),
            source_path: Some(self.path.expect("path not set")),
        })
    }
}

impl From<ServiceBuilder> for cm_rust::CapabilityDecl {
    fn from(builder: ServiceBuilder) -> Self {
        builder.build()
    }
}

// A convenience builder for constructing DirectoryDecls.
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct DirectoryBuilder {
    name: Option<Name>,
    path: Option<Path>,
    #[derivative(Default(value = "fio::R_STAR_DIR"))]
    rights: fio::Operations,
}

impl DirectoryBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        self
    }

    pub fn path(mut self, path: &str) -> Self {
        self.path = Some(path.parse().unwrap());
        self
    }

    pub fn rights(mut self, rights: fio::Operations) -> Self {
        self.rights = rights;
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        cm_rust::CapabilityDecl::Directory(cm_rust::DirectoryDecl {
            name: self.name.expect("name not set"),
            source_path: Some(self.path.expect("path not set")),
            rights: self.rights,
        })
    }
}

impl From<DirectoryBuilder> for cm_rust::CapabilityDecl {
    fn from(builder: DirectoryBuilder) -> Self {
        builder.build()
    }
}

// A convenience builder for constructing StorageDecls.
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct StorageBuilder {
    name: Option<Name>,
    backing_dir: Option<Name>,
    subdir: Option<PathBuf>,
    source: Option<cm_rust::StorageDirectorySource>,
    #[derivative(Default(value = "fdecl::StorageId::StaticInstanceIdOrMoniker"))]
    storage_id: fdecl::StorageId,
}

impl StorageBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        self
    }

    pub fn backing_dir(mut self, backing_dir: &str) -> Self {
        self.backing_dir = Some(backing_dir.parse().unwrap());
        self
    }

    pub fn source(mut self, source: cm_rust::StorageDirectorySource) -> Self {
        self.source = Some(source);
        self
    }

    pub fn subdir(mut self, subdir: &str) -> Self {
        self.subdir = Some(subdir.parse().unwrap());
        self
    }

    pub fn storage_id(mut self, storage_id: fdecl::StorageId) -> Self {
        self.storage_id = storage_id;
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        cm_rust::CapabilityDecl::Storage(cm_rust::StorageDecl {
            name: self.name.expect("name not set"),
            backing_dir: self.backing_dir.expect("backing_dir not set"),
            source: self.source.expect("source not set"),
            subdir: self.subdir,
            storage_id: self.storage_id,
        })
    }
}

impl From<StorageBuilder> for cm_rust::CapabilityDecl {
    fn from(builder: StorageBuilder) -> Self {
        builder.build()
    }
}

// A convenience builder for constructing RunnerDecls.
#[derive(Debug, Default)]
pub struct RunnerBuilder {
    name: Option<Name>,
    path: Option<Path>,
}

impl RunnerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Defaults `source_path` to `/svc/fuchsia.component.runner.ComponentRunner`.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        if self.path.is_none() {
            self.path =
                Some(format!("/svc/fuchsia.component.runner.ComponentRunner").parse().unwrap());
        }
        self
    }

    pub fn path(mut self, path: &str) -> Self {
        self.path = Some(path.parse().unwrap());
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        cm_rust::CapabilityDecl::Runner(cm_rust::RunnerDecl {
            name: self.name.expect("name not set"),
            source_path: Some(self.path.expect("path not set")),
        })
    }
}

impl From<RunnerBuilder> for cm_rust::CapabilityDecl {
    fn from(builder: RunnerBuilder) -> Self {
        builder.build()
    }
}

// A convenience builder for constructing ResolverDecls.
#[derive(Debug, Default)]
pub struct ResolverBuilder {
    name: Option<Name>,
    path: Option<Path>,
}

impl ResolverBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Defaults `source_path` to `/svc/fuchsia.component.resolution.Resolver`.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        if self.path.is_none() {
            self.path =
                Some(format!("/svc/fuchsia.component.resolution.Resolver").parse().unwrap());
        }
        self
    }

    pub fn path(mut self, path: &str) -> Self {
        self.path = Some(path.parse().unwrap());
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        cm_rust::CapabilityDecl::Resolver(cm_rust::ResolverDecl {
            name: self.name.expect("name not set"),
            source_path: Some(self.path.expect("path not set")),
        })
    }
}

impl From<ResolverBuilder> for cm_rust::CapabilityDecl {
    fn from(builder: ResolverBuilder) -> Self {
        builder.build()
    }
}
