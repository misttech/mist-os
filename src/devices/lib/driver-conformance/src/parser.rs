// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use fidl_fuchsia_driver_framework as fdf;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

#[derive(Deserialize, Debug)]
pub struct PlaceholderEmptyDict {}

#[derive(Deserialize, Debug, PartialEq, Clone, Eq, Hash)]
pub struct DeviceCategory {
    pub category: String,
    pub subcategory: String,
}

impl FromStr for DeviceCategory {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let vec = value.split("::").collect::<Vec<&str>>();
        match vec.len() {
            1 => {
                let val = vec[0].trim();
                if val.is_empty() {
                    Err(anyhow::anyhow!("Provided category is empty."))
                } else {
                    Ok(Self { category: val.to_string(), subcategory: "".to_string() })
                }
            }
            2 => {
                let val0 = vec[0].trim();
                let val1 = vec[1].trim();
                if val0.is_empty() || val1.is_empty() {
                    Err(anyhow::anyhow!("Provided category is empty."))
                } else {
                    Ok(Self { category: val0.to_string(), subcategory: val1.to_string() })
                }
            }
            _ => Err(anyhow::anyhow!(
                "'{}' is an invalid category. Only expecting at most one '::'.",
                value.to_string()
            )),
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TestInfo {
    #[allow(unused)]
    pub url: String,
    #[allow(unused)]
    pub test_types: Box<[String]>,
    pub device_categories: Box<[DeviceCategory]>,
    #[allow(unused)]
    pub is_automated: bool,
}

#[derive(Deserialize, Debug, Default)]
pub struct TestMetadata {
    #[allow(unused)]
    pub certification_type: HashMap<String, PlaceholderEmptyDict>,
    #[allow(unused)]
    pub system_types: HashMap<String, PlaceholderEmptyDict>,
    #[allow(unused)]
    pub driver_test_types: HashMap<String, PlaceholderEmptyDict>,
    pub device_category_types: HashMap<String, HashMap<String, PlaceholderEmptyDict>>,
    pub tests: Box<[TestInfo]>,
}

pub trait ValidateAgainstMetadata {
    fn validate_device_categories(&self, list: &[DeviceCategory]) -> Result<()>;
}

impl ValidateAgainstMetadata for TestMetadata {
    fn validate_device_categories(&self, list: &[DeviceCategory]) -> Result<()> {
        for cat in list {
            #[allow(clippy::map_entry)] // TODO(https://fxbug.dev/407086469)
            if self.device_category_types.contains_key(&cat.category) {
                if !cat.subcategory.is_empty()
                    && !self.device_category_types[&cat.category].contains_key(&cat.subcategory)
                {
                    return Err(anyhow!(
                        "{}::{} is not a valid category.",
                        &cat.category,
                        &cat.subcategory
                    ));
                }
            } else {
                return Err(anyhow!(
                    "{}::{} is not a valid category.",
                    &cat.category,
                    &cat.subcategory
                ));
            }
        }
        Ok(())
    }
}

/// Helper functions to filter the tests in the metadata by various criteria.
pub trait FilterTests {
    fn tests_by_url(&self, filter: &[String]) -> Result<HashSet<TestInfo>>;
    fn tests_by_test_type(&self, filter: &[String]) -> Result<HashSet<TestInfo>>;
    fn tests_by_device_category(&self, filter: &[DeviceCategory]) -> Result<HashSet<TestInfo>>;
    fn tests_by_driver(&self, driver: &fdf::DriverInfo) -> Result<HashSet<TestInfo>>;
}

impl FilterTests for TestMetadata {
    /// Gets list of tests based on a provided list of test URLs.
    ///
    /// Will error if a given URL does not match a known test.
    fn tests_by_url(&self, urls: &[String]) -> Result<HashSet<TestInfo>> {
        let mut ret = HashSet::new();
        for url in urls.iter() {
            let mut found = false;
            for test in self.tests.iter() {
                if &test.url == url {
                    found = true;
                    ret.insert(test.clone());
                }
            }
            if !found {
                return Err(anyhow!("The URL '{}' does not match any test.", &url));
            }
        }
        Ok(ret)
    }

    /// WIP: Gets list of tests based on a provided list of test types.
    fn tests_by_test_type(&self, _types: &[String]) -> Result<HashSet<TestInfo>> {
        Err(anyhow!("This function is not yet implemented."))
    }

    /// Gets list of tests based on a provided list of device categories.
    ///
    /// The given categories must be known categories.
    /// Tests must only have categories that fit within the set of given categories.
    fn tests_by_device_category(
        &self,
        device_categories: &[DeviceCategory],
    ) -> Result<HashSet<TestInfo>> {
        let mut ret = HashSet::new();
        for item in device_categories.iter() {
            // Validate that the device categories exist.
            if !self.device_category_types.contains_key(&item.category) {
                return Err(anyhow!(
                    "The metadata does not define a category '{}'",
                    &item.category
                ));
            }
            if !&item.subcategory.is_empty()
                && !self.device_category_types[&item.category].contains_key(&item.subcategory)
            {
                return Err(anyhow!(
                    "The category '{}' does not contain the subcategory '{}'",
                    &item.category,
                    &item.subcategory
                ));
            }
            for test in self.tests.iter() {
                let mut skip = false;
                for cat in test.device_categories.iter() {
                    if !device_categories.contains(cat) {
                        skip = true;
                        break;
                    }
                }
                if !skip {
                    ret.insert(test.clone());
                }
            }
        }
        Ok(ret)
    }

    /// Gets list of tests that can be run against the given driver.
    fn tests_by_driver(&self, driver: &fdf::DriverInfo) -> Result<HashSet<TestInfo>> {
        if let Some(device_categories) = &driver.device_categories {
            let mut category_list: HashSet<DeviceCategory> = HashSet::new();
            for cats in device_categories.iter() {
                match (&cats.category, &cats.subcategory) {
                    (Some(category), Some(subcategory)) => {
                        category_list.insert(DeviceCategory {
                            category: category.to_string(),
                            subcategory: subcategory.to_string(),
                        });
                    }
                    (Some(category), None) => {
                        category_list.insert(DeviceCategory {
                            category: category.to_string(),
                            subcategory: "".to_string(),
                        });
                    }
                    _ => {
                        if let Some(url) = &driver.url {
                            return Err(anyhow!(
                                "There is an empty device category entry for driver {}",
                                url
                            ));
                        } else {
                            return Err(anyhow!(
                                "There is an empty device category entry for the given driver."
                            ));
                        }
                    }
                }
            }
            return Ok(
                self.tests_by_device_category(&category_list.into_iter().collect::<Vec<_>>()[..])?
            );
        }
        return Err(anyhow!("The provided driver contains no device categories."));
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use crate::*;

    pub fn init_metadata() -> parser::TestMetadata {
        serde_json::from_str(
            r#"{
            "certification_type": {
            },
            "system_types": {
            },
            "driver_test_types": {
              "functional": {},
              "performance": {}
            },
            "device_category_types": {
              "imaging": {
                "camera": {}
              },
              "connectivity": {
                "wifi": {},
                "ethernet": {}
              },
              "input": {
                "touchpad": {},
                "touchscreen": {},
                "keyboard": {},
                "mouse": {}
              },
              "misc": {}
            },
            "tests": [
              {
                "url": "fuchsia-pkg://a/b#meta/c.cm",
                "test_types": [
                  "functional"
                ],
                "device_categories": [
                  {
                    "category": "imaging",
                    "subcategory": "camera"
                  }
                ],
                "is_automated": true
              },
              {
                "url": "fuchsia-pkg://a/d#meta/e.cm",
                "test_types": [
                  "functional"
                ],
                "device_categories": [
                  {
                    "category": "misc",
                    "subcategory": ""
                  }
                ],
                "is_automated": false
              },
              {
                "url": "fuchsia-pkg://a/f#meta/g.cm",
                "test_types": [
                  "functional"
                ],
                "device_categories": [
                  {
                    "category": "imaging",
                    "subcategory": "camera"
                  },
                  {
                    "category": "input",
                    "subcategory": "touchpad"
                  }
                ],
                "is_automated": true
              }
            ]
          }"#,
        )
        .unwrap()
    }

    #[test]
    fn test_tests_by_url() {
        let meta = init_metadata();

        let test0 = meta.tests_by_url(&["fuchsia-pkg://a/f#meta/g.cm".to_string()]);
        assert!(test0.is_ok());
        let test0u = test0.unwrap();
        assert_eq!(test0u.len(), 1);
        assert_eq!(
            test0u,
            HashSet::from([parser::TestInfo {
                url: "fuchsia-pkg://a/f#meta/g.cm".to_string(),
                test_types: Box::new(["functional".to_string()]),
                device_categories: Box::new([
                    parser::DeviceCategory {
                        category: "imaging".to_string(),
                        subcategory: "camera".to_string()
                    },
                    parser::DeviceCategory {
                        category: "input".to_string(),
                        subcategory: "touchpad".to_string()
                    }
                ]),
                is_automated: true
            }])
        );

        let test1 = meta.tests_by_url(&[
            "fuchsia-pkg://a/f#meta/g.cm".to_string(),
            "fuchsia-pkg://a/b#meta/c.cm".to_string(),
            "fuchsia-pkg://a/d#meta/e.cm".to_string(),
        ]);
        assert!(test1.is_ok());
        assert_eq!(test1.unwrap().len(), 3);

        let test2 = meta.tests_by_url(&["fuchsia-pkg://a/f#meta/wrong.cm".to_string()]);
        assert!(test2.is_err());

        let test3 = meta.tests_by_url(&[]);
        assert!(test3.is_ok());
        assert_eq!(test3.unwrap().len(), 0);
    }

    #[test]
    fn test_tests_by_device_category() {
        let meta = init_metadata();

        let test0 = meta.tests_by_device_category(&[parser::DeviceCategory {
            category: "imaging".to_string(),
            subcategory: "camera".to_string(),
        }]);
        assert!(test0.is_ok());
        assert_eq!(test0.unwrap().len(), 1);

        let test1 = meta.tests_by_device_category(&[parser::DeviceCategory {
            category: "misc".to_string(),
            subcategory: "".to_string(),
        }]);
        assert!(test1.is_ok());
        assert_eq!(test1.unwrap().len(), 1);

        let test2 = meta.tests_by_device_category(&[
            parser::DeviceCategory {
                category: "input".to_string(),
                subcategory: "touchpad".to_string(),
            },
            parser::DeviceCategory {
                category: "imaging".to_string(),
                subcategory: "camera".to_string(),
            },
        ]);
        assert!(test2.is_ok());
        assert_eq!(test2.unwrap().len(), 2);

        let test3 = meta.tests_by_device_category(&[parser::DeviceCategory {
            category: "input".to_string(),
            subcategory: "mouse".to_string(),
        }]);
        assert!(test3.is_ok());
        assert_eq!(test3.unwrap().len(), 0);

        let test4 = meta.tests_by_device_category(&[parser::DeviceCategory {
            category: "bike".to_string(),
            subcategory: "wheels".to_string(),
        }]);
        assert!(test4.is_err());

        let test5 = meta.tests_by_device_category(&[parser::DeviceCategory {
            category: "input".to_string(),
            subcategory: "pogo".to_string(),
        }]);
        assert!(test5.is_err());
    }

    #[test]
    fn test_tests_by_driver() {
        let meta = init_metadata();

        let test0 = meta.tests_by_driver(&fdf::DriverInfo::default());
        assert!(test0.is_err());

        let test1 = meta.tests_by_driver(&fdf::DriverInfo {
            device_categories: Some(vec![fdf::DeviceCategory::default()]),
            ..Default::default()
        });
        assert!(test1.is_err());

        let test2 = meta.tests_by_driver(&fdf::DriverInfo {
            device_categories: Some(vec![fdf::DeviceCategory {
                category: Some("misc".to_string()),
                subcategory: None,
                ..Default::default()
            }]),
            ..Default::default()
        });
        assert!(test2.is_ok());
        assert_eq!(test2.unwrap().len(), 1);

        let test3 = meta.tests_by_driver(&fdf::DriverInfo {
            device_categories: Some(vec![
                fdf::DeviceCategory {
                    category: Some("input".to_string()),
                    subcategory: Some("touchpad".to_string()),
                    ..Default::default()
                },
                fdf::DeviceCategory {
                    category: Some("imaging".to_string()),
                    subcategory: Some("camera".to_string()),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        });
        assert!(test3.is_ok());
        assert_eq!(test3.unwrap().len(), 2);

        let test4 = meta.tests_by_driver(&fdf::DriverInfo {
            device_categories: Some(vec![fdf::DeviceCategory {
                category: Some("bike".to_string()),
                subcategory: None,
                ..Default::default()
            }]),
            ..Default::default()
        });
        assert!(test4.is_err());

        let test5 = meta.tests_by_driver(&fdf::DriverInfo {
            device_categories: Some(vec![fdf::DeviceCategory {
                category: Some("input".to_string()),
                subcategory: Some("pogo".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        });
        assert!(test5.is_err());
    }

    #[test]
    fn test_test_device_category_from_str_ok() {
        let test0 = "a::b";
        let cat0 = parser::DeviceCategory::from_str(test0).unwrap();
        assert_eq!(cat0.category, "a");
        assert_eq!(cat0.subcategory, "b");

        let test1 = "a";
        let cat1 = parser::DeviceCategory::from_str(test1).unwrap();
        assert_eq!(cat1.category, "a");
        assert_eq!(cat1.subcategory, "");
    }

    #[test]
    fn test_test_device_category_from_str_err() {
        let empty = "";
        let mut result = parser::DeviceCategory::from_str(empty);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Provided category is empty.");

        let space = " ";
        result = parser::DeviceCategory::from_str(space);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Provided category is empty.");

        let empty_cat = "::b";
        result = parser::DeviceCategory::from_str(empty_cat);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Provided category is empty.");

        let empty_sub = "a::";
        result = parser::DeviceCategory::from_str(empty_sub);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Provided category is empty.");

        let trio = "a::b::c";
        result = parser::DeviceCategory::from_str(trio);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "'a::b::c' is an invalid category. Only expecting at most one '::'."
        );
    }

    #[test]
    fn test_validate_device_categories_ok() {
        let metadata = init_metadata();

        let single = [parser::DeviceCategory {
            category: "imaging".to_string(),
            subcategory: "camera".to_string(),
        }];
        assert!(metadata.validate_device_categories(&single).is_ok());

        let multiple = [
            parser::DeviceCategory {
                category: "imaging".to_string(),
                subcategory: "camera".to_string(),
            },
            parser::DeviceCategory {
                category: "input".to_string(),
                subcategory: "mouse".to_string(),
            },
        ];
        assert!(metadata.validate_device_categories(&multiple).is_ok());
    }

    #[test]
    fn test_validate_device_categories_err() {
        let metadata = init_metadata();

        let single =
            [parser::DeviceCategory { category: "a".to_string(), subcategory: "b".to_string() }];
        match metadata.validate_device_categories(&single) {
            Ok(_) => assert!(false, "This call should not pass."),
            Err(e) => assert_eq!(e.to_string(), "a::b is not a valid category."),
        }

        let multiple = [
            parser::DeviceCategory {
                category: "imaging".to_string(),
                subcategory: "camera".to_string(),
            },
            parser::DeviceCategory { category: "c".to_string(), subcategory: "d".to_string() },
        ];
        match metadata.validate_device_categories(&multiple) {
            Ok(_) => assert!(false, "This call should not pass."),
            Err(e) => assert_eq!(e.to_string(), "c::d is not a valid category."),
        }
    }
}
