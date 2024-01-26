// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::testing::routing_test_helpers::*, cm_rust::*, cm_rust_testing::*,
    routing_test_helpers::RoutingTestModel,
};

#[fuchsia::test]
async fn use_protocol_from_dictionary() {
    // Test extracting a protocol from a dictionary with source parent, self and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("parent_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("parent_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "parent_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "parent_dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("parent_dict".parse().unwrap()),
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("leaf".into()),
                    source_name: "C".parse().unwrap(),
                    source_dictionary: Some("child_dict".parse().unwrap()),
                    target_path: "/svc/C".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B", "/svc/C"] {
        test.check_use(
            vec!["mid"].try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn offer_protocol_from_dictionary() {
    // Test extracting a protocol from a dictionary with source parent, self, and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("parent_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("parent_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Dictionary(OfferDictionaryDecl {
                    source: OfferSource::Self_,
                    source_name: "parent_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "parent_dict".parse().unwrap(),
                    target: OfferTarget::static_child("mid".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_name: "A_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Parent,
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("parent_dict".parse().unwrap()),
                    target_name: "B_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::static_child("provider".into()),
                    source_name: "C".parse().unwrap(),
                    source_dictionary: Some("child_dict".parse().unwrap()),
                    target_name: "C_svc".parse().unwrap(),
                    target: OfferTarget::static_child("leaf".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("provider".into()))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "provider",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "C".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "A_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "B_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Parent,
                    source_name: "C_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/C".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B", "/svc/C"] {
        test.check_use(
            vec!["mid", "leaf"].try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn expose_protocol_from_dictionary() {
    // Test extracting a protocol from a dictionary with source self and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("mid".into()),
                    source_name: "A_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/A".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .use_(UseDecl::Protocol(UseProtocolDecl {
                    dependency_type: DependencyType::Strong,
                    source: UseSource::Child("mid".into()),
                    source_name: "B_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_path: "/svc/B".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("mid".into()))
                .build(),
        ),
        (
            "mid",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Self_,
                    source_name: "A".parse().unwrap(),
                    source_dictionary: Some("self_dict".parse().unwrap()),
                    target_name: "A_svc".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Child("leaf".into()),
                    source_name: "B".parse().unwrap(),
                    source_dictionary: Some("child_dict".parse().unwrap()),
                    target_name: "B_svc".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    for path in ["/svc/A", "/svc/B"] {
        test.check_use(
            vec![].try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}

#[fuchsia::test]
async fn dictionary_in_exposed_dir() {
    // Test extracting a protocol from a dictionary with source self and child.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("self_dict").build())
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "self_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "self_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Child("leaf".into()),
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "A".parse().unwrap(),
                    target: OfferTarget::Capability("self_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .add_child(ChildDeclBuilder::new_lazy_child("leaf".into()))
                .build(),
        ),
        (
            "leaf",
            ComponentDeclBuilder::new()
                .protocol(ProtocolDeclBuilder::new("foo_svc").path("/svc/foo").build())
                .dictionary(DictionaryDeclBuilder::new("child_dict").build())
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo_svc".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "B".parse().unwrap(),
                    target: OfferTarget::Capability("child_dict".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .expose(ExposeDecl::Dictionary(ExposeDictionaryDecl {
                    source: ExposeSource::Self_,
                    source_name: "child_dict".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "child_dict".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: Availability::Required,
                }))
                .build(),
        ),
    ];

    let test = RoutingTestBuilder::new("root", components).build().await;
    // The dictionaries in the exposed dir will be converted to subdirectories.
    for path in ["/self_dict/A", "/child_dict/B"] {
        test.check_use_exposed_dir(
            vec![].try_into().unwrap(),
            CheckUse::Protocol { path: path.parse().unwrap(), expected_res: ExpectedResult::Ok },
        )
        .await;
    }
}
