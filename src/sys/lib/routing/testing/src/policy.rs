// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    assert_matches::assert_matches,
    cm_moniker::InstancedMoniker,
    cm_rust::{CapabilityTypeName, ProtocolDecl, StorageDecl, StorageDirectorySource},
    fidl_fuchsia_component_decl as fdecl,
    moniker::{ExtendedMoniker, Moniker},
    routing::{
        capability_source::{CapabilitySource, ComponentCapability, InternalCapability},
        component_instance::ComponentInstanceInterface,
        config::{
            AllowlistEntry, AllowlistEntryBuilder, CapabilityAllowlistKey,
            CapabilityAllowlistSource, ChildPolicyAllowlists, DebugCapabilityAllowlistEntry,
            DebugCapabilityKey, JobPolicyAllowlists, SecurityPolicy,
        },
        policy::GlobalPolicyChecker,
    },
    std::{
        collections::{HashMap, HashSet},
        iter::FromIterator,
        sync::{Arc, Weak},
    },
};

/// These GlobalPolicyChecker tests are run under multiple contexts, e.g. both on Fuchsia under
/// component_manager and on the build host under cm_fidl_analyzer. This macro helps ensure that all
/// tests are run in each context.
#[macro_export]
macro_rules! instantiate_global_policy_checker_tests {
    ($fixture_impl:path) => {
        // New GlobalPolicyCheckerTest tests must be added to this list to run.
        instantiate_global_policy_checker_tests! {
            $fixture_impl,
            global_policy_checker_can_route_capability_framework_cap,
            global_policy_checker_can_route_capability_namespace_cap,
            global_policy_checker_can_route_capability_component_cap,
            global_policy_checker_can_route_capability_capability_cap,
            global_policy_checker_can_route_debug_capability_capability_cap,
            global_policy_checker_can_route_debug_capability_with_realm_allowlist_entry,
            global_policy_checker_can_route_debug_capability_with_collection_allowlist_entry,
            global_policy_checker_can_route_capability_builtin_cap,
            global_policy_checker_can_route_capability_with_realm_allowlist_entry,
            global_policy_checker_can_route_capability_with_collection_allowlist_entry,
        }
    };
    ($fixture_impl:path, $test:ident, $($remaining:ident),+ $(,)?) => {
        instantiate_global_policy_checker_tests! { $fixture_impl, $test }
        instantiate_global_policy_checker_tests! { $fixture_impl, $($remaining),+ }
    };
    ($fixture_impl:path, $test:ident) => {
        #[test]
        fn $test() -> Result<(), Error> {
            <$fixture_impl as Default>::default().$test()
        }
    };
}

// Tests `GlobalPolicyChecker` for implementations of `ComponentInstanceInterface`.
pub trait GlobalPolicyCheckerTest<C>
where
    C: ComponentInstanceInterface,
{
    // Creates a `ComponentInstanceInterface` with the given `InstancedMoniker`.
    fn make_component(&self, instanced_moniker: InstancedMoniker) -> Arc<C>;

    // Tests `GlobalPolicyChecker::can_route_capability()` for framework capability sources.
    fn global_policy_checker_can_route_capability_framework_cap(&self) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(
                    Moniker::try_from(vec!["foo", "bar"]).unwrap(),
                ),
                source_name: "fuchsia.component.Realm".parse().unwrap(),
                source: CapabilityAllowlistSource::Framework,
                capability: CapabilityTypeName::Protocol,
            },
            vec![
                AllowlistEntryBuilder::new().exact("foo").exact("bar").build(),
                AllowlistEntryBuilder::new().exact("foo").exact("bar").exact("baz").build(),
            ],
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));
        let component = self.make_component(vec!["foo:0", "bar:0"].try_into().unwrap());

        let protocol_capability = CapabilitySource::<C>::Framework {
            capability: InternalCapability::Protocol("fuchsia.component.Realm".parse().unwrap()),
            component: component.as_weak(),
        };
        let valid_path_0 = Moniker::try_from(vec!["foo", "bar"]).unwrap();
        let valid_path_1 = Moniker::try_from(vec!["foo", "bar", "baz"]).unwrap();
        let invalid_path_0 = Moniker::try_from(vec!["foobar"]).unwrap();
        let invalid_path_1 = Moniker::try_from(vec!["foo", "bar", "foobar"]).unwrap();

        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &valid_path_0),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &valid_path_1),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &invalid_path_0),
            Err(_)
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &invalid_path_1),
            Err(_)
        );
        Ok(())
    }

    // Tests `GlobalPolicyChecker::can_route_capability()` for namespace capability sources.
    fn global_policy_checker_can_route_capability_namespace_cap(&self) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentManager,
                source_name: "fuchsia.boot.RootResource".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            vec![
                AllowlistEntryBuilder::new().exact("root").build(),
                AllowlistEntryBuilder::new().exact("root").exact("bootstrap").build(),
                AllowlistEntryBuilder::new().exact("root").exact("core").build(),
            ],
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));

        let protocol_capability = CapabilitySource::<C>::Namespace {
            capability: ComponentCapability::Protocol(ProtocolDecl {
                name: "fuchsia.boot.RootResource".parse().unwrap(),
                source_path: Some("/svc/fuchsia.boot.RootResource".parse().unwrap()),
            }),
            top_instance: Weak::new(),
        };
        let valid_path_0 = Moniker::try_from(vec!["root"]).unwrap();
        let valid_path_2 = Moniker::try_from(vec!["root", "core"]).unwrap();
        let valid_path_1 = Moniker::try_from(vec!["root", "bootstrap"]).unwrap();
        let invalid_path_0 = Moniker::try_from(vec!["foobar"]).unwrap();
        let invalid_path_1 = Moniker::try_from(vec!["foo", "bar", "foobar"]).unwrap();

        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &valid_path_0),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &valid_path_1),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &valid_path_2),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &invalid_path_0),
            Err(_)
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &invalid_path_1),
            Err(_)
        );
        Ok(())
    }

    // Tests `GlobalPolicyChecker::can_route_capability()` for component capability sources.
    fn global_policy_checker_can_route_capability_component_cap(&self) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(
                    Moniker::try_from(vec!["foo"]).unwrap(),
                ),
                source_name: "fuchsia.foo.FooBar".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            vec![
                AllowlistEntryBuilder::new().exact("foo").build(),
                AllowlistEntryBuilder::new().exact("root").exact("bootstrap").build(),
                AllowlistEntryBuilder::new().exact("root").exact("core").build(),
            ],
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));
        let component = self.make_component(vec!["foo:0"].try_into().unwrap());

        let protocol_capability = CapabilitySource::<C>::Component {
            capability: ComponentCapability::Protocol(ProtocolDecl {
                name: "fuchsia.foo.FooBar".parse().unwrap(),
                source_path: Some("/svc/fuchsia.foo.FooBar".parse().unwrap()),
            }),
            component: component.as_weak(),
        };
        let valid_path_0 = Moniker::try_from(vec!["root", "bootstrap"]).unwrap();
        let valid_path_1 = Moniker::try_from(vec!["root", "core"]).unwrap();
        let invalid_path_0 = Moniker::try_from(vec!["foobar"]).unwrap();
        let invalid_path_1 = Moniker::try_from(vec!["foo", "bar", "foobar"]).unwrap();

        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &valid_path_0),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &valid_path_1),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &invalid_path_0),
            Err(_)
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &invalid_path_1),
            Err(_)
        );
        Ok(())
    }

    // Tests `GlobalPolicyChecker::can_route_capability()` for capability sources of type `Capability`.
    fn global_policy_checker_can_route_capability_capability_cap(&self) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(
                    Moniker::try_from(vec!["foo"]).unwrap(),
                ),
                source_name: "cache".parse().unwrap(),
                source: CapabilityAllowlistSource::Capability,
                capability: CapabilityTypeName::Storage,
            },
            vec![
                AllowlistEntryBuilder::new().exact("foo").build(),
                AllowlistEntryBuilder::new().exact("root").exact("bootstrap").build(),
                AllowlistEntryBuilder::new().exact("root").exact("core").build(),
            ],
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));
        let component = self.make_component(vec!["foo:0"].try_into().unwrap());

        let protocol_capability = CapabilitySource::<C>::Capability {
            source_capability: ComponentCapability::Storage(StorageDecl {
                backing_dir: "cache".parse().unwrap(),
                name: "cache".parse().unwrap(),
                source: StorageDirectorySource::Parent,
                subdir: None,
                storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
            }),
            component: component.as_weak(),
        };
        let valid_path_0 = Moniker::try_from(vec!["root", "bootstrap"]).unwrap();
        let valid_path_1 = Moniker::try_from(vec!["root", "core"]).unwrap();
        let invalid_path_0 = Moniker::try_from(vec!["foobar"]).unwrap();
        let invalid_path_1 = Moniker::try_from(vec!["foo", "bar", "foobar"]).unwrap();

        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &valid_path_0),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &valid_path_1),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &invalid_path_0),
            Err(_)
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&protocol_capability, &invalid_path_1),
            Err(_)
        );
        Ok(())
    }

    // Tests `GlobalPolicyChecker::can_route_debug_capability()` for capability sources of type `Capability`.
    fn global_policy_checker_can_route_debug_capability_capability_cap(&self) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "debug_service1".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "foo_env".to_string(),
            },
            AllowlistEntryBuilder::new().exact("foo").build(),
            AllowlistEntryBuilder::new().exact("foo").build(),
        );
        policy_builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "debug_service1".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "bootstrap_env".to_string(),
            },
            AllowlistEntryBuilder::new().exact("foo").build(),
            AllowlistEntryBuilder::new().exact("root").exact("bootstrap").build(),
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));
        let component = self.make_component(vec!["foo:0"].try_into().unwrap());

        let protocol_capability = CapabilitySource::<C>::Component {
            capability: ComponentCapability::Protocol(ProtocolDecl {
                name: "debug_service1".parse().unwrap(),
                source_path: Some("/svc/debug_service1".parse().unwrap()),
            }),
            component: component.as_weak(),
        };

        let valid_cases = vec![
            (Moniker::try_from(vec!["root", "bootstrap"]).unwrap(), "bootstrap_env".to_string()),
            (Moniker::try_from(vec!["foo"]).unwrap(), "foo_env".to_string()),
        ];

        let invalid_cases = vec![
            (Moniker::try_from(vec!["foobar"]).unwrap(), "foobar_env".to_string()),
            (Moniker::try_from(vec!["foo", "bar", "foobar"]).unwrap(), "foobar_env".to_string()),
            (Moniker::try_from(vec!["root", "bootstrap"]).unwrap(), "foo_env".to_string()),
            (Moniker::try_from(vec!["root", "baz"]).unwrap(), "foo_env".to_string()),
        ];

        let target_moniker = Moniker::try_from(vec!["target"]).unwrap();

        for valid_case in valid_cases {
            assert_matches!(
                global_policy_checker.can_route_debug_capability(
                    &protocol_capability,
                    &valid_case.0,
                    &valid_case.1,
                    &target_moniker
                ),
                Ok(()),
                "{:?}",
                valid_case
            );
        }

        for invalid_case in invalid_cases {
            assert_matches!(
                global_policy_checker.can_route_debug_capability(
                    &protocol_capability,
                    &invalid_case.0,
                    &invalid_case.1,
                    &target_moniker
                ),
                Err(_),
                "{:?}",
                invalid_case
            );
        }

        Ok(())
    }

    // Tests `GlobalPolicyChecker::can_route_debug_capability()` for capability sources of type
    // `Capability` with realm allowlist entries.
    fn global_policy_checker_can_route_debug_capability_with_realm_allowlist_entry(
        &self,
    ) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "debug_service1".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "bar_env".to_string(),
            },
            AllowlistEntryBuilder::new().exact("bar").build(),
            AllowlistEntryBuilder::new().exact("root").exact("bootstrap1").any_descendant(),
        );
        policy_builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "debug_service1".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "foo_env".to_string(),
            },
            AllowlistEntryBuilder::new().exact("foo").any_descendant(),
            AllowlistEntryBuilder::new().exact("root").exact("bootstrap2").build(),
        );
        policy_builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "debug_service1".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "baz_env".to_string(),
            },
            AllowlistEntryBuilder::new().exact("baz").any_descendant(),
            AllowlistEntryBuilder::new().exact("root").exact("bootstrap3").any_descendant(),
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));

        // source, dest, env
        let valid_cases = vec![
            (vec!["bar:0"], vec!["root", "bootstrap1", "child"], "bar_env".to_string()),
            (
                vec!["bar:0"],
                vec!["root", "bootstrap1", "child", "grandchild"],
                "bar_env".to_string(),
            ),
            (vec!["foo:0", "child:0"], vec!["root", "bootstrap2"], "foo_env".to_string()),
            (
                vec!["foo:0", "child:0", "grandchild:0"],
                vec!["root", "bootstrap2"],
                "foo_env".to_string(),
            ),
            (vec!["baz:0", "child:0"], vec!["root", "bootstrap3", "child"], "baz_env".to_string()),
            (
                vec!["baz:0", "child:0", "granchild:0"],
                vec!["root", "bootstrap3", "child", "grandchild"],
                "baz_env".to_string(),
            ),
        ];

        let invalid_cases = vec![
            (vec!["bar:0"], vec!["root", "bootstrap"], "bar_env".to_string()),
            (vec!["bar:0"], vec!["root", "not_bootstrap"], "bar_env".to_string()),
            (vec!["foo:0"], vec!["root", "bootstrap2"], "foo_env".to_string()),
            (vec!["foo:0", "child:0"], vec!["root", "not_bootstrap"], "foo_env".to_string()),
            (vec!["baz:0"], vec!["root", "bootstrap3", "child"], "baz_env".to_string()),
            (vec!["baz:0", "child:0"], vec!["root", "bootstrap"], "baz_env".to_string()),
        ];

        let target_moniker = Moniker::try_from(vec!["target"]).unwrap();

        for (source, dest, env) in valid_cases {
            let component = self.make_component(source.clone().try_into().unwrap());
            let protocol_capability = CapabilitySource::<C>::Component {
                capability: ComponentCapability::Protocol(ProtocolDecl {
                    name: "debug_service1".parse().unwrap(),
                    source_path: Some("/svc/debug_service1".parse().unwrap()),
                }),
                component: component.as_weak(),
            };
            assert_matches!(
                global_policy_checker.can_route_debug_capability(
                    &protocol_capability,
                    &Moniker::try_from(dest.clone()).unwrap(),
                    &env,
                    &target_moniker
                ),
                Ok(()),
                "{:?}",
                (source, dest, env)
            );
        }

        for (source, dest, env) in invalid_cases {
            let component = self.make_component(source.clone().try_into().unwrap());
            let protocol_capability = CapabilitySource::<C>::Component {
                capability: ComponentCapability::Protocol(ProtocolDecl {
                    name: "debug_service1".parse().unwrap(),
                    source_path: Some("/svc/debug_service1".parse().unwrap()),
                }),
                component: component.as_weak(),
            };
            assert_matches!(
                global_policy_checker.can_route_debug_capability(
                    &protocol_capability,
                    &Moniker::try_from(dest.clone()).unwrap(),
                    &env,
                    &target_moniker
                ),
                Err(_),
                "{:?}",
                (source, dest, env)
            );
        }

        Ok(())
    }

    // Tests `GlobalPolicyChecker::can_route_debug_capability()` for capability sources of type
    // `Capability` with collection allowlist entries.
    fn global_policy_checker_can_route_debug_capability_with_collection_allowlist_entry(
        &self,
    ) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "debug_service1".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "bar_env".to_string(),
            },
            AllowlistEntryBuilder::new().exact("bar").build(),
            AllowlistEntryBuilder::new()
                .exact("root")
                .exact("bootstrap")
                .any_descendant_in_collection("coll1"),
        );
        policy_builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "debug_service1".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "foo_env".to_string(),
            },
            AllowlistEntryBuilder::new().exact("foo").any_descendant_in_collection("coll2"),
            AllowlistEntryBuilder::new().exact("root").exact("bootstrap2").build(),
        );
        policy_builder.add_debug_capability_policy(
            DebugCapabilityKey {
                source_name: "debug_service1".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
                env_name: "baz_env".to_string(),
            },
            AllowlistEntryBuilder::new().exact("baz").any_descendant_in_collection("coll3"),
            AllowlistEntryBuilder::new()
                .exact("root")
                .exact("bootstrap3")
                .any_descendant_in_collection("coll4"),
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));

        // source, dest, env
        let valid_cases = vec![
            (vec!["bar:0"], vec!["root", "bootstrap", "coll1:instance1"], "bar_env".to_string()),
            (
                vec!["bar:0"],
                vec!["root", "bootstrap", "coll1:instance1", "child"],
                "bar_env".to_string(),
            ),
            (vec!["foo:0", "coll2:instance2:0"], vec!["root", "bootstrap2"], "foo_env".to_string()),
            (
                vec!["foo:0", "coll2:instance2:0", "child:0"],
                vec!["root", "bootstrap2"],
                "foo_env".to_string(),
            ),
            (
                vec!["baz:0", "coll3:instance3:0"],
                vec!["root", "bootstrap3", "coll4:instance4"],
                "baz_env".to_string(),
            ),
            (
                vec!["baz:0", "coll3:instance3:0", "child:0"],
                vec!["root", "bootstrap3", "coll4:instance4", "child"],
                "baz_env".to_string(),
            ),
        ];

        let invalid_cases = vec![
            (vec!["bar:0"], vec!["root", "bootstrap"], "bar_env".to_string()),
            (vec!["bar:0"], vec!["root", "not_bootstrap"], "bar_env".to_string()),
            (vec!["foo:0"], vec!["root", "bootstrap2"], "foo_env".to_string()),
            (vec!["foo:0", "child:0"], vec!["root", "not_bootstrap"], "foo_env".to_string()),
            (vec!["baz:0"], vec!["root", "bootstrap3", "child"], "baz_env".to_string()),
            (vec!["baz:0", "child:0"], vec!["root", "bootstrap"], "baz_env".to_string()),
        ];

        let target_moniker = Moniker::try_from(vec!["target"]).unwrap();

        for (source, dest, env) in valid_cases {
            let component = self.make_component(source.clone().try_into().unwrap());
            let protocol_capability = CapabilitySource::<C>::Component {
                capability: ComponentCapability::Protocol(ProtocolDecl {
                    name: "debug_service1".parse().unwrap(),
                    source_path: Some("/svc/debug_service1".parse().unwrap()),
                }),
                component: component.as_weak(),
            };
            assert_matches!(
                global_policy_checker.can_route_debug_capability(
                    &protocol_capability,
                    &Moniker::try_from(dest.clone()).unwrap(),
                    &env,
                    &target_moniker
                ),
                Ok(()),
                "{:?}",
                (source, dest, env)
            );
        }

        for (source, dest, env) in invalid_cases {
            let component = self.make_component(source.clone().try_into().unwrap());
            let protocol_capability = CapabilitySource::<C>::Component {
                capability: ComponentCapability::Protocol(ProtocolDecl {
                    name: "debug_service1".parse().unwrap(),
                    source_path: Some("/svc/debug_service1".parse().unwrap()),
                }),
                component: component.as_weak(),
            };
            assert_matches!(
                global_policy_checker.can_route_debug_capability(
                    &protocol_capability,
                    &Moniker::try_from(dest.clone()).unwrap(),
                    &env,
                    &target_moniker
                ),
                Err(_),
                "{:?}",
                (source, dest, env)
            );
        }

        Ok(())
    }

    // Tests `GlobalPolicyChecker::can_route_capability()` for builtin capabilities.
    fn global_policy_checker_can_route_capability_builtin_cap(&self) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentManager,
                source_name: "test".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Directory,
            },
            vec![
                AllowlistEntryBuilder::new().exact("root").build(),
                AllowlistEntryBuilder::new().exact("root").exact("core").build(),
            ],
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));

        let dir_capability = CapabilitySource::<C>::Builtin {
            capability: InternalCapability::Directory("test".parse().unwrap()),
            top_instance: Weak::new(),
        };
        let valid_path_0 = Moniker::try_from(vec!["root"]).unwrap();
        let valid_path_1 = Moniker::try_from(vec!["root", "core"]).unwrap();
        let invalid_path_0 = Moniker::try_from(vec!["foobar"]).unwrap();
        let invalid_path_1 = Moniker::try_from(vec!["foo", "bar", "foobar"]).unwrap();

        assert_matches!(
            global_policy_checker.can_route_capability(&dir_capability, &valid_path_0),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&dir_capability, &valid_path_1),
            Ok(())
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&dir_capability, &invalid_path_0),
            Err(_)
        );
        assert_matches!(
            global_policy_checker.can_route_capability(&dir_capability, &invalid_path_1),
            Err(_)
        );
        Ok(())
    }

    // Tests `GlobalPolicyChecker::can_route_capability()` for policy that includes non-exact
    // `AllowlistEntry::Realm` entries.
    fn global_policy_checker_can_route_capability_with_realm_allowlist_entry(
        &self,
    ) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentManager,
                source_name: "fuchsia.boot.RootResource".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            vec![
                AllowlistEntryBuilder::new().exact("tests").any_descendant(),
                AllowlistEntryBuilder::new().exact("core").exact("tests").any_descendant(),
            ],
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));
        let protocol_capability = CapabilitySource::<C>::Namespace {
            capability: ComponentCapability::Protocol(ProtocolDecl {
                name: "fuchsia.boot.RootResource".parse().unwrap(),
                source_path: Some("/svc/fuchsia.boot.RootResource".parse().unwrap()),
            }),
            top_instance: Weak::new(),
        };

        macro_rules! can_route {
            ($moniker:expr) => {
                global_policy_checker.can_route_capability(&protocol_capability, $moniker)
            };
        }

        assert!(can_route!(&Moniker::try_from(vec!["tests", "test1"]).unwrap()).is_ok());
        assert!(can_route!(&Moniker::try_from(vec!["tests", "coll:test1"]).unwrap()).is_ok());
        assert!(can_route!(&Moniker::try_from(vec!["tests", "test1", "util"]).unwrap()).is_ok());
        assert!(can_route!(&Moniker::try_from(vec!["tests", "test2"]).unwrap()).is_ok());
        assert!(can_route!(&Moniker::try_from(vec!["core", "tests", "test"]).unwrap()).is_ok());
        assert!(can_route!(&Moniker::try_from(vec!["core", "tests", "coll:t"]).unwrap()).is_ok());

        assert!(can_route!(&Moniker::try_from(vec!["foo"]).unwrap()).is_err());
        assert!(can_route!(&Moniker::try_from(vec!["tests"]).unwrap()).is_err());
        assert!(can_route!(&Moniker::try_from(vec!["core", "foo"]).unwrap()).is_err());
        assert!(can_route!(&Moniker::try_from(vec!["core", "tests"]).unwrap()).is_err());
        assert!(can_route!(&Moniker::try_from(vec!["core", "tests:test"]).unwrap()).is_err());
        Ok(())
    }

    // Tests `GlobalPolicyChecker::can_route_capability()` for policy that includes non-exact
    // `AllowlistEntry::Collection` entries.
    fn global_policy_checker_can_route_capability_with_collection_allowlist_entry(
        &self,
    ) -> Result<(), Error> {
        let mut policy_builder = CapabilityAllowlistPolicyBuilder::new();
        policy_builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentManager,
                source_name: "fuchsia.boot.RootResource".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            vec![
                AllowlistEntryBuilder::new().any_descendant_in_collection("tests"),
                AllowlistEntryBuilder::new().exact("core").any_descendant_in_collection("tests"),
            ],
        );
        let global_policy_checker = GlobalPolicyChecker::new(Arc::new(policy_builder.build()));
        let protocol_capability = CapabilitySource::<C>::Namespace {
            capability: ComponentCapability::Protocol(ProtocolDecl {
                name: "fuchsia.boot.RootResource".parse().unwrap(),
                source_path: Some("/svc/fuchsia.boot.RootResource".parse().unwrap()),
            }),
            top_instance: Weak::new(),
        };

        macro_rules! can_route {
            ($moniker:expr) => {
                global_policy_checker.can_route_capability(&protocol_capability, $moniker)
            };
        }

        assert!(can_route!(&Moniker::try_from(vec!["tests:t1"]).unwrap()).is_ok());
        assert!(can_route!(&Moniker::try_from(vec!["tests:t2"]).unwrap()).is_ok());
        assert!(can_route!(&Moniker::try_from(vec!["tests:t1", "util"]).unwrap()).is_ok());
        assert!(can_route!(&Moniker::try_from(vec!["core", "tests:t1"]).unwrap()).is_ok());
        assert!(can_route!(&Moniker::try_from(vec!["core", "tests:t2"]).unwrap()).is_ok());

        assert!(can_route!(&Moniker::try_from(vec!["foo"]).unwrap()).is_err());
        assert!(can_route!(&Moniker::try_from(vec!["tests"]).unwrap()).is_err());
        assert!(can_route!(&Moniker::try_from(vec!["coll:foo"]).unwrap()).is_err());
        assert!(can_route!(&Moniker::try_from(vec!["core", "foo"]).unwrap()).is_err());
        assert!(can_route!(&Moniker::try_from(vec!["core", "coll:tests"]).unwrap()).is_err());
        Ok(())
    }
}

// Creates a SecurityPolicy based on the capability allowlist entries provided during
// construction.
struct CapabilityAllowlistPolicyBuilder {
    capability_policy: HashMap<CapabilityAllowlistKey, HashSet<AllowlistEntry>>,
    debug_capability_policy: HashMap<DebugCapabilityKey, HashSet<DebugCapabilityAllowlistEntry>>,
}

impl CapabilityAllowlistPolicyBuilder {
    pub fn new() -> Self {
        Self { capability_policy: HashMap::new(), debug_capability_policy: HashMap::new() }
    }

    /// Add a new entry to the configuration.
    pub fn add_capability_policy<'a>(
        &'a mut self,
        key: CapabilityAllowlistKey,
        value: Vec<AllowlistEntry>,
    ) -> &'a mut Self {
        let value_set = HashSet::from_iter(value.iter().cloned());
        self.capability_policy.insert(key, value_set);
        self
    }

    /// Add a new entry to the configuration.
    pub fn add_debug_capability_policy<'a>(
        &'a mut self,
        key: DebugCapabilityKey,
        source: AllowlistEntry,
        dest: AllowlistEntry,
    ) -> &'a mut Self {
        self.debug_capability_policy
            .entry(key)
            .or_default()
            .insert(DebugCapabilityAllowlistEntry::new(source, dest));
        self
    }

    /// Creates a configuration from the provided policies.
    pub fn build(&self) -> SecurityPolicy {
        SecurityPolicy {
            job_policy: JobPolicyAllowlists {
                ambient_mark_vmo_exec: vec![],
                main_process_critical: vec![],
                create_raw_processes: vec![],
            },
            capability_policy: self.capability_policy.clone(),
            debug_capability_policy: self.debug_capability_policy.clone(),
            child_policy: ChildPolicyAllowlists { reboot_on_terminate: vec![] },
            ..Default::default()
        }
    }
}
