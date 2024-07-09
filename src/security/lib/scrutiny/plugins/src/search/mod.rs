// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod controller;

#[cfg(test)]
mod tests {
    use crate::core::collection::testing::fake_component_src_pkg;
    use crate::core::collection::{Component, Components, Package, Packages};
    use crate::search::controller::components::{
        ComponentSearchController, ComponentSearchRequest,
    };
    use crate::search::controller::packages::{PackageSearchController, PackageSearchRequest};
    use fuchsia_merkle::{Hash, HASH_SIZE};
    use fuchsia_url::{AbsolutePackageUrl, PackageName};
    use scrutiny::model::controller::DataController;
    use scrutiny::model::model::DataModel;
    use scrutiny_testing::fake::fake_data_model;
    use scrutiny_testing::TEST_REPO_URL;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::str::FromStr;
    use std::sync::Arc;

    fn data_model() -> Arc<DataModel> {
        fake_data_model()
    }

    #[fuchsia::test]
    fn test_component_search() {
        let model = data_model();
        let search = ComponentSearchController::default();
        model
            .set(Components::new(vec![Component {
                id: 0,
                url: cm_types::Url::new(
                    &AbsolutePackageUrl::new(
                        TEST_REPO_URL.clone(),
                        PackageName::from_str("foo").unwrap(),
                        None,
                        None,
                    )
                    .to_string(),
                )
                .unwrap(),
                source: fake_component_src_pkg(),
            }]))
            .unwrap();
        let request_one = ComponentSearchRequest { url: "foo".to_string() };
        let request_two = ComponentSearchRequest { url: "bar".to_string() };
        let response_one: Vec<Component> =
            serde_json::from_value(search.query(model.clone(), json!(request_one)).unwrap())
                .unwrap();
        let response_two: Vec<Component> =
            serde_json::from_value(search.query(model.clone(), json!(request_two)).unwrap())
                .unwrap();
        assert_eq!(response_one.len(), 1);
        assert_eq!(response_two.len(), 0);
    }

    #[fuchsia::test]
    fn test_package_search() {
        let model = data_model();
        let search = PackageSearchController::default();
        let mut contents = HashMap::new();
        contents.insert(PathBuf::from("foo"), Hash::from([0; HASH_SIZE]));
        model
            .set(Packages::new(vec![Package {
                name: PackageName::from_str("test_name").unwrap(),
                variant: None,
                merkle: Hash::from([0; HASH_SIZE]),
                contents,
                meta: HashMap::new(),
            }]))
            .unwrap();
        let request_one = PackageSearchRequest { files: "foo".to_string() };
        let request_two = PackageSearchRequest { files: "bar".to_string() };
        let response_one: Vec<Package> =
            serde_json::from_value(search.query(model.clone(), json!(request_one)).unwrap())
                .unwrap();
        let response_two: Vec<Package> =
            serde_json::from_value(search.query(model.clone(), json!(request_two)).unwrap())
                .unwrap();
        assert_eq!(response_one.len(), 1);
        assert_eq!(response_two.len(), 0);
    }
}
