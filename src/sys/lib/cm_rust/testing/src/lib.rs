// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    cm_rust::{ComponentDecl, FidlIntoNative},
    cm_types::{Name, Path, RelativePath},
    cml, fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio,
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
    pub fn add_child(mut self, decl: impl Into<cm_rust::ChildDecl>) -> Self {
        self.result.children.push(decl.into());
        self
    }

    // Add a collection element.
    pub fn add_collection(mut self, decl: impl Into<cm_rust::CollectionDecl>) -> Self {
        self.result.collections.push(decl.into());
        self
    }

    /// Add a lazily instantiated child with a default test URL derived from the name.
    pub fn add_lazy_child(self, name: &str) -> Self {
        self.add_child(ChildDeclBuilder::new_lazy_child(name))
    }

    /// Add an eagerly instantiated child with a default test URL derived from the name.
    pub fn add_eager_child(self, name: &str) -> Self {
        self.add_child(
            ChildDeclBuilder::new()
                .name(name)
                .url(&format!("test:///{}", name))
                .startup(fdecl::StartupMode::Eager),
        )
    }

    /// Add a transient collection.
    pub fn add_transient_collection(self, name: &str) -> Self {
        self.add_collection(CollectionDeclBuilder::new_transient_collection(name))
    }

    /// Add a single run collection.
    pub fn add_single_run_collection(self, name: &str) -> Self {
        self.add_collection(CollectionDeclBuilder::new_single_run_collection(name))
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
    pub fn capability(mut self, capability: cm_rust::CapabilityDecl) -> Self {
        self.result.capabilities.push(capability);
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
    pub fn add_environment(mut self, environment: impl Into<cm_rust::EnvironmentDecl>) -> Self {
        self.result.environments.push(environment.into());
        self
    }

    /// Add a config declaration.
    pub fn add_config(mut self, config: cm_rust::ConfigDecl) -> Self {
        self.result.config = Some(config);
        self
    }

    /// Generate the final ComponentDecl.
    pub fn build(self) -> ComponentDecl {
        self.result
    }
}

/// A convenience builder for constructing ChildDecls.
#[derive(Debug)]
pub struct ChildDeclBuilder(cm_rust::ChildDecl);

impl ChildDeclBuilder {
    /// Creates a new builder.
    pub fn new() -> Self {
        ChildDeclBuilder(cm_rust::ChildDecl {
            name: String::new(),
            url: String::new(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        })
    }

    /// Creates a new builder initialized with a lazy child.
    pub fn new_lazy_child(name: &str) -> Self {
        Self::new().name(name).url(&format!("test:///{}", name)).startup(fdecl::StartupMode::Lazy)
    }

    /// Sets the ChildDecl's name.
    pub fn name(mut self, name: &str) -> Self {
        self.0.name = name.to_string();
        self
    }

    /// Sets the ChildDecl's url.
    pub fn url(mut self, url: &str) -> Self {
        self.0.url = url.to_string();
        self
    }

    /// Sets the ChildDecl's startup mode.
    pub fn startup(mut self, startup: fdecl::StartupMode) -> Self {
        self.0.startup = startup;
        self
    }

    /// Sets the ChildDecl's on_terminate action.
    pub fn on_terminate(mut self, on_terminate: fdecl::OnTerminate) -> Self {
        self.0.on_terminate = Some(on_terminate);
        self
    }

    /// Sets the ChildDecl's environment name.
    pub fn environment(mut self, environment: &str) -> Self {
        self.0.environment = Some(environment.to_string());
        self
    }

    /// Consumes the builder and returns a ChildDecl.
    pub fn build(mut self) -> cm_rust::ChildDecl {
        if self.0.url == String::new() {
            self.0.url = format!("test://{}", self.0.name);
        }
        self.0
    }
}

impl From<ChildDeclBuilder> for cm_rust::ChildDecl {
    fn from(builder: ChildDeclBuilder) -> Self {
        builder.build()
    }
}

/// A convenience builder for constructing CollectionDecls.
#[derive(Debug)]
pub struct CollectionDeclBuilder(cm_rust::CollectionDecl);

impl CollectionDeclBuilder {
    /// Creates a new builder.
    pub fn new() -> Self {
        CollectionDeclBuilder(cm_rust::CollectionDecl {
            name: "coll".parse().unwrap(),
            durability: fdecl::Durability::Transient,
            environment: None,
            allowed_offers: cm_types::AllowedOffers::StaticOnly,
            allow_long_names: false,
            persistent_storage: None,
        })
    }

    /// Creates a new builder initialized with a transient collection.
    pub fn new_transient_collection(name: &str) -> Self {
        Self::new().name(name).durability(fdecl::Durability::Transient)
    }

    /// Creates a new builder initialized with a single run collection.
    pub fn new_single_run_collection(name: &str) -> Self {
        Self::new().name(name).durability(fdecl::Durability::SingleRun)
    }

    /// Sets the CollectionDecl's name.
    pub fn name(mut self, name: &str) -> Self {
        self.0.name = name.parse().expect("invalid name");
        self
    }

    /// Sets the CollectionDecl's durability
    pub fn durability(mut self, durability: fdecl::Durability) -> Self {
        self.0.durability = durability;
        self
    }

    /// Sets the CollectionDecl's environment name.
    pub fn environment(mut self, environment: &str) -> Self {
        self.0.environment = Some(environment.to_string());
        self
    }

    /// Sets the kinds of offers that may target the instances in the
    /// collection.
    pub fn allowed_offers(mut self, allowed_offers: cm_types::AllowedOffers) -> Self {
        self.0.allowed_offers = allowed_offers;
        self
    }

    // Sets the flag to allow the collection to have child names that exceed the default length
    // limit.
    pub fn allow_long_names(mut self, allow_long_names: bool) -> Self {
        self.0.allow_long_names = allow_long_names;
        self
    }

    // Sets the flag to persist isolated storage data of the collection and its descendents.
    pub fn persistent_storage(mut self, persistent_storage: bool) -> Self {
        self.0.persistent_storage = Some(persistent_storage);
        self
    }

    /// Consumes the builder and returns a CollectionDecl.
    pub fn build(self) -> cm_rust::CollectionDecl {
        self.0
    }
}

impl From<CollectionDeclBuilder> for cm_rust::CollectionDecl {
    fn from(builder: CollectionDeclBuilder) -> Self {
        builder.build()
    }
}

/// A convenience builder for constructing EnvironmentDecls.
#[derive(Debug)]
pub struct EnvironmentDeclBuilder(cm_rust::EnvironmentDecl);

impl EnvironmentDeclBuilder {
    /// Creates a new builder.
    pub fn new() -> Self {
        EnvironmentDeclBuilder(cm_rust::EnvironmentDecl {
            name: String::new(),
            extends: fdecl::EnvironmentExtends::None,
            runners: vec![],
            resolvers: vec![],
            debug_capabilities: vec![],
            stop_timeout_ms: None,
        })
    }

    /// Sets the EnvironmentDecl's name.
    pub fn name(mut self, name: &str) -> Self {
        self.0.name = name.to_string();
        self
    }

    /// Sets whether the environment extends from its realm.
    pub fn extends(mut self, extends: fdecl::EnvironmentExtends) -> Self {
        self.0.extends = extends;
        self
    }

    /// Registers a runner with the environment.
    pub fn add_runner(mut self, runner: cm_rust::RunnerRegistration) -> Self {
        self.0.runners.push(runner);
        self
    }

    /// Registers a resolver with the environment.
    pub fn add_resolver(mut self, resolver: cm_rust::ResolverRegistration) -> Self {
        self.0.resolvers.push(resolver);
        self
    }

    /// Registers a debug capability with the environment.
    pub fn add_debug_registration(mut self, debug: cm_rust::DebugRegistration) -> Self {
        self.0.debug_capabilities.push(debug);
        self
    }

    pub fn stop_timeout(mut self, timeout_ms: u32) -> Self {
        self.0.stop_timeout_ms = Some(timeout_ms);
        self
    }

    /// Consumes the builder and returns an EnvironmentDecl.
    pub fn build(self) -> cm_rust::EnvironmentDecl {
        self.0
    }
}

impl From<EnvironmentDeclBuilder> for cm_rust::EnvironmentDecl {
    fn from(builder: EnvironmentDeclBuilder) -> Self {
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
#[derive(Debug, Default)]
pub struct DirectoryBuilder {
    name: Option<Name>,
    path: Option<Path>,
    rights: Option<fio::Operations>,
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
        self.rights = Some(rights);
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        cm_rust::CapabilityDecl::Directory(cm_rust::DirectoryDecl {
            name: self.name.expect("name not set"),
            source_path: Some(self.path.expect("path not set")),
            rights: self.rights.unwrap_or(fio::R_STAR_DIR),
        })
    }
}

impl From<DirectoryBuilder> for cm_rust::CapabilityDecl {
    fn from(builder: DirectoryBuilder) -> Self {
        builder.build()
    }
}

// A convenience builder for constructing StorageDecls.
#[derive(Debug, Default)]
pub struct StorageBuilder {
    name: Option<Name>,
    backing_dir: Option<Name>,
    subdir: Option<PathBuf>,
    source: Option<cm_rust::StorageDirectorySource>,
    storage_id: Option<fdecl::StorageId>,
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
        self.storage_id = Some(storage_id);
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        cm_rust::CapabilityDecl::Storage(cm_rust::StorageDecl {
            name: self.name.expect("name not set"),
            backing_dir: self.backing_dir.expect("backing_dir not set"),
            source: self.source.expect("source not set"),
            subdir: self.subdir,
            storage_id: self.storage_id.unwrap_or(fdecl::StorageId::StaticInstanceIdOrMoniker),
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
