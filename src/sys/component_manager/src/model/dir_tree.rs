// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::WeakComponentInstance,
        error::VfsError,
        mutable_directory::MutableDirectory,
        routing::{self, RouteRequest},
    },
    ::routing::capability_source::ComponentCapability,
    cm_rust::{CapabilityPath, CapabilityTypeName, ComponentDecl, ExposeDecl, UseDecl},
    fidl_fuchsia_io as fio,
    std::collections::HashMap,
    std::sync::Arc,
    vfs::directory::immutable::simple as pfs,
    vfs::remote::{self, RoutingFn},
};

/// Represents the directory hierarchy of the exposed directory, not including the nodes for the
/// capabilities themselves.
pub struct DirTree {
    directory_nodes: HashMap<String, Box<DirTree>>,
    broker_nodes: HashMap<String, (RoutingFn, fio::DirentType)>,
}

impl DirTree {
    /// Builds a directory hierarchy from a component's `uses` declarations.
    /// `routing_factory` is a closure that generates the routing function that will be called
    /// when a leaf node is opened.
    pub fn build_from_uses(
        routing_factory: impl Fn(WeakComponentInstance, ComponentCapability, RouteRequest) -> RoutingFn,
        component: WeakComponentInstance,
        decl: &ComponentDecl,
    ) -> Self {
        let mut tree = DirTree { directory_nodes: HashMap::new(), broker_nodes: HashMap::new() };
        for use_ in &decl.uses {
            tree.add_use_capability(&routing_factory, component.clone(), use_);
        }
        tree
    }

    /// Builds a directory hierarchy from a component's `exposes` declarations.
    /// `routing_factory` is a closure that generates the routing function that will be called
    /// when a leaf node is opened.
    pub fn build_from_exposes(
        routing_factory: impl Fn(WeakComponentInstance, ComponentCapability, RouteRequest) -> RoutingFn,
        component: WeakComponentInstance,
        decl: &ComponentDecl,
    ) -> Self {
        let mut tree = DirTree { directory_nodes: HashMap::new(), broker_nodes: HashMap::new() };
        // Group exposes by target name because some capabilities (services) support aggregation,
        // where multiple declarations represent one aggregated capability route.
        let exposes_by_target_name = routing::aggregate_exposes(&decl.exposes);
        for (target_name, exposes) in exposes_by_target_name {
            tree.add_expose_capability(&routing_factory, component.clone(), target_name, exposes);
        }
        tree
    }

    /// Installs the directory tree into `root_dir`.
    pub fn install<'entries>(self, root_dir: &mut Arc<pfs::Simple>) -> Result<(), VfsError> {
        for (name, subtree) in self.directory_nodes {
            let mut subdir = pfs::simple();
            subtree.install(&mut subdir)?;
            root_dir.add_node(&name, subdir)?;
        }
        for (name, value) in self.broker_nodes {
            let (route_fn, dirent_type) = value;
            let node = remote::remote_boxed_with_type(route_fn, dirent_type);
            root_dir.add_node(&name, node)?;
        }
        Ok(())
    }

    fn add_use_capability(
        &mut self,
        routing_factory: &impl Fn(WeakComponentInstance, ComponentCapability, RouteRequest) -> RoutingFn,
        component: WeakComponentInstance,
        use_: &UseDecl,
    ) {
        let cap = ComponentCapability::Use(use_.clone());
        let request = match routing::request_for_namespace_capability_use(use_.clone()) {
            Some(r) => r,
            None => return,
        };
        let path = match use_.path() {
            Some(path) => path.clone(),
            None => return,
        };
        let type_name = cap.type_name();
        let routing_fn = routing_factory(component, cap, request);
        self.add_capability(path, type_name, routing_fn);
    }

    fn add_expose_capability(
        &mut self,
        routing_factory: &impl Fn(WeakComponentInstance, ComponentCapability, RouteRequest) -> RoutingFn,
        component: WeakComponentInstance,
        target_name: &str,
        exposes: Vec<&ExposeDecl>,
    ) {
        let path: CapabilityPath =
            format!("/{}", target_name).parse().expect("couldn't parse name as path");
        // If there are multiple exposes, choosing the first expose for `cap`. `cap` is only used
        // for debug info.
        //
        // TODO(fxbug.dev/4776): This could lead to incomplete debug output because the source name
        // is what's printed, so if the exposes have different source names only one of them will
        // appear in the output. However, in practice routing is unlikely to fail for an aggregate
        // because the algorithm typically terminates once an aggregate is found. Find a more robust
        // solution, such as including all exposes or switching to the target name.
        let first_expose = *exposes.first().expect("empty exposes is impossible");
        let cap = ComponentCapability::Expose(first_expose.clone());
        let type_name = cap.type_name();
        let request = match routing::request_for_namespace_capability_expose(exposes) {
            Some(r) => r,
            None => return,
        };
        let routing_fn = routing_factory(component, cap, request);
        self.add_capability(path, type_name, routing_fn);
    }

    fn add_capability(
        &mut self,
        path: CapabilityPath,
        _type_name: CapabilityTypeName,
        routing_fn: RoutingFn,
    ) {
        // TODO(fxbug.dev/126066): Don't set this to Unknown, set it based on the type_name.
        let dirent_type = fio::DirentType::Unknown;
        let tree = self.to_directory_node(&path);
        tree.broker_nodes.insert(path.basename.to_string(), (routing_fn, dirent_type));
    }

    fn to_directory_node(&mut self, path: &CapabilityPath) -> &mut DirTree {
        let components = path.dirname.split("/");
        let mut tree = self;
        for component in components {
            if !component.is_empty() {
                tree = tree.directory_nodes.entry(component.to_string()).or_insert(Box::new(
                    DirTree { directory_nodes: HashMap::new(), broker_nodes: HashMap::new() },
                ));
            }
        }
        tree
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::{
            component::ComponentInstance,
            context::ModelContext,
            environment::Environment,
            testing::{mocks, test_helpers, test_helpers::*},
        },
        ::routing::component_instance::ComponentInstanceInterface,
        cm_rust::{
            Availability, CapabilityPath, DependencyType, ExposeDecl, ExposeDirectoryDecl,
            ExposeProtocolDecl, ExposeRunnerDecl, ExposeServiceDecl, ExposeSource, ExposeTarget,
            UseDecl, UseDirectoryDecl, UseProtocolDecl, UseSource, UseStorageDecl,
        },
        fidl::endpoints::{ClientEnd, ServerEnd},
        fidl_fuchsia_io as fio, fuchsia_zircon as zx,
        std::{
            convert::{TryFrom, TryInto},
            sync::{Arc, Weak},
        },
        vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path},
    };

    #[fuchsia::test]
    async fn read_only_storage() {
        // Call `build_from_uses` with a routing factory that routes to a mock directory or service,
        // and a `ComponentDecl` with `use` declarations.
        let routing_factory = mocks::proxy_routing_factory(mocks::DeclType::Use);
        let decl = ComponentDecl {
            uses: vec![UseDecl::Storage(UseStorageDecl {
                source_name: "data".parse().unwrap(),
                target_path: "/data".try_into().unwrap(),
                availability: Availability::Required,
            })],
            ..default_component_decl()
        };
        let root = ComponentInstance::new_root(
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "test://root".to_string(),
        );
        let tree = DirTree::build_from_uses(routing_factory, root.as_weak(), &decl);

        // Convert the tree to a directory.
        let mut in_dir = pfs::simple();
        tree.install(&mut in_dir).expect("Unable to build pseudodirectory");

        // Ensure that we can't create a file if the permission is read-only
        let (data_dir, data_server) = zx::Channel::create();
        in_dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            path::Path::validate_and_split("data").unwrap(),
            ServerEnd::<fio::NodeMarker>::new(data_server.into()),
        );
        let data_dir = ClientEnd::<fio::DirectoryMarker>::new(data_dir.into());
        let data_dir = data_dir.into_proxy().unwrap();
        let error = fuchsia_fs::directory::open_file(
            &data_dir,
            "test",
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE,
        )
        .await
        .unwrap_err();
        match error {
            fuchsia_fs::node::OpenError::OpenError(zx::Status::ACCESS_DENIED) => {}
            e => panic!("Unexpected open error: {}", e),
        }
    }

    #[fuchsia::test]
    async fn build_from_uses() {
        // Call `build_from_uses` with a routing factory that routes to a mock directory or service,
        // and a `ComponentDecl` with `use` declarations.
        let routing_factory = mocks::proxy_routing_factory(mocks::DeclType::Use);
        let decl = ComponentDecl {
            uses: vec![
                UseDecl::Directory(UseDirectoryDecl {
                    source: UseSource::Parent,
                    source_name: "baz-dir".parse().unwrap(),
                    target_path: CapabilityPath::try_from("/in/data/hippo").unwrap(),
                    rights: fio::Operations::CONNECT,
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }),
                UseDecl::Protocol(UseProtocolDecl {
                    source: UseSource::Parent,
                    source_name: "baz-svc".parse().unwrap(),
                    target_path: CapabilityPath::try_from("/in/svc/hippo").unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }),
                UseDecl::Storage(UseStorageDecl {
                    source_name: "data".parse().unwrap(),
                    target_path: "/in/data/persistent".try_into().unwrap(),
                    availability: Availability::Required,
                }),
                UseDecl::Storage(UseStorageDecl {
                    source_name: "cache".parse().unwrap(),
                    target_path: "/in/data/cache".try_into().unwrap(),
                    availability: Availability::Required,
                }),
            ],
            ..default_component_decl()
        };
        let root = ComponentInstance::new_root(
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "test://root".to_string(),
        );
        let tree = DirTree::build_from_uses(routing_factory, root.as_weak(), &decl);

        // Convert the tree to a directory.
        let mut in_dir = pfs::simple();
        tree.install(&mut in_dir).expect("Unable to build pseudodirectory");
        let (in_dir_client, in_dir_server) = zx::Channel::create();
        in_dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            path::Path::dot(),
            ServerEnd::<fio::NodeMarker>::new(in_dir_server.into()),
        );
        let in_dir_proxy = ClientEnd::<fio::DirectoryMarker>::new(in_dir_client)
            .into_proxy()
            .expect("failed to create directory proxy");
        assert_eq!(
            vec!["in/data/cache", "in/data/hippo", "in/data/persistent", "in/svc/hippo"],
            test_helpers::list_directory_recursive(&in_dir_proxy).await
        );

        // Expect that calls on the directory nodes reach the mock directory/service.
        assert_eq!("friend", test_helpers::read_file(&in_dir_proxy, "in/data/hippo/hello").await);
        assert_eq!(
            "friend",
            test_helpers::read_file(&in_dir_proxy, "in/data/persistent/hello").await
        );
        assert_eq!("friend", test_helpers::read_file(&in_dir_proxy, "in/data/cache/hello").await);
        assert_eq!(
            "hippos".to_string(),
            test_helpers::call_echo(&in_dir_proxy, "in/svc/hippo", "hippos").await
        );
    }

    #[fuchsia::test]
    async fn build_from_exposes() {
        // Call `build_from_exposes` with a routing factory that routes to a mock directory or
        // protocol, and a `ComponentDecl` with `expose` declarations.
        let routing_factory = mocks::proxy_routing_factory(mocks::DeclType::Expose);
        let decl = ComponentDecl {
            exposes: vec![
                ExposeDecl::Directory(ExposeDirectoryDecl {
                    source: ExposeSource::Self_,
                    source_name: "baz-dir".parse().unwrap(),
                    target_name: "hippo-dir".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    rights: Some(fio::Operations::CONNECT),
                    subdir: None,
                    availability: cm_rust::Availability::Required,
                }),
                ExposeDecl::Directory(ExposeDirectoryDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo-dir".parse().unwrap(),
                    target_name: "bar-dir".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    rights: Some(fio::Operations::CONNECT),
                    subdir: None,
                    availability: cm_rust::Availability::Required,
                }),
                ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Self_,
                    source_name: "baz-proto".parse().unwrap(),
                    target_name: "hippo-proto".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }),
                // Aggregated service from two declarations.
                ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo-svc".parse().unwrap(),
                    target_name: "whale-svc".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }),
                ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "bar-svc".parse().unwrap(),
                    target_name: "whale-svc".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }),
                ExposeDecl::Runner(ExposeRunnerDecl {
                    source: ExposeSource::Self_,
                    source_name: "elf".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    target_name: "elf".parse().unwrap(),
                }),
            ],
            ..default_component_decl()
        };
        let root = ComponentInstance::new_root(
            Environment::empty(),
            Arc::new(ModelContext::new_for_test()),
            Weak::new(),
            "test://root".to_string(),
        );
        let tree = DirTree::build_from_exposes(routing_factory, root.as_weak(), &decl);

        // Convert the tree to a directory.
        let mut expose_dir = pfs::simple();
        tree.install(&mut expose_dir).expect("Unable to build pseudodirectory");
        let (expose_dir_client, expose_dir_server) = zx::Channel::create();
        expose_dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            path::Path::dot(),
            ServerEnd::<fio::NodeMarker>::new(expose_dir_server.into()),
        );
        let expose_dir_proxy = ClientEnd::<fio::DirectoryMarker>::new(expose_dir_client)
            .into_proxy()
            .expect("failed to create directory proxy");
        assert_eq!(
            vec!["bar-dir", "hippo-dir", "hippo-proto", "whale-svc"],
            test_helpers::list_directory_recursive(&expose_dir_proxy).await
        );

        // Expect that calls on the directory nodes reach the mock directory/service.
        assert_eq!("friend", test_helpers::read_file(&expose_dir_proxy, "bar-dir/hello").await);
        assert_eq!("friend", test_helpers::read_file(&expose_dir_proxy, "hippo-dir/hello").await);
        assert_eq!(
            "hippos".to_string(),
            test_helpers::call_echo(&expose_dir_proxy, "hippo-proto", "hippos").await
        );
        assert_eq!(
            "whales".to_string(),
            test_helpers::call_echo(&expose_dir_proxy, "whale-svc/default/echo", "whales").await
        );
    }
}
