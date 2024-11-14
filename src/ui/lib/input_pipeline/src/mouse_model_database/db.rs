// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{data, data_import_from_chromiumos};
use fidl_fuchsia_input_report as fidl_input_report;
use lazy_static::lazy_static;
use std::collections::HashMap;

/// Mouse have a sensor that tells how far they moved in "counts", depends
/// on sensor, mouse will report different CPI (counts per inch). Currently,
/// "standard" mouse is 1000 CPI, and it can up to 8000 CPI. We need this
/// database to understand how far the mouse moved.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MouseModel {
    pub(crate) identifier: &'static str,
    pub(crate) vendor_id: &'static str,
    pub(crate) product_id: &'static str,
    /// Some device has multiple CPI, just give the default CPI.
    pub(crate) counts_per_mm: u32,
}

const MM_PER_INCH: f32 = 25.4;
pub(crate) const DEFAULT_COUNTS_PER_MM: u32 = (1000.0 / MM_PER_INCH) as u32;

impl MouseModel {
    fn new(
        vendor_id: &'static str,
        product_id: &'static str,
        counts_per_inch: u32,
        identifier: &'static str,
    ) -> Self {
        MouseModel {
            identifier,
            vendor_id,
            product_id,
            counts_per_mm: ((counts_per_inch as f32) / MM_PER_INCH) as u32,
        }
    }
}

/// contains structures help product id matches in one vendor.
/// The matching order is:
/// 1. match exact model first.
/// 2. match by glob pattern, patterns would not conflict because pattern only
///    allow 3 hex digits with 1 tailing *, and no duplicated pattern ensured
///    by `validate_vendor_id_product_id`.
/// 3. use default model of vendor.
struct VendorProducts {
    default_model: Option<MouseModel>,
    patterns: Vec<(glob::Pattern, MouseModel)>,
    exact_models: HashMap<String, MouseModel>,
}

impl VendorProducts {
    fn new() -> Self {
        Self { default_model: None, patterns: vec![], exact_models: HashMap::new() }
    }

    fn add(&mut self, model: MouseModel) {
        if model.product_id == "*" {
            // 1 vendor can only have 1 wildcard matching. this panic will
            // not be happen in production because panic will be caught
            // in test `validate_vendor_id_product_id`.
            assert_eq!(self.default_model, None);
            self.default_model = Some(model);
        } else if model.product_id.ends_with("*") {
            // This panic will not be reached in production because all panic
            // will be caught in test `validate_vendor_id_product_id`.
            let pattern =
                glob::Pattern::new(model.product_id).expect("product id is not a valid glob");
            self.patterns.push((pattern, model));
        } else {
            self.exact_models.insert(model.product_id.to_string(), model);
        }
    }

    fn get(&self, product_id: u32) -> Option<MouseModel> {
        let pid = to_hex(product_id);
        if let Some(m) = self.exact_models.get(&pid) {
            return Some(m.clone());
        }

        if let Some((_, m)) = self.patterns.iter().find(|(p, _)| p.matches(&pid)) {
            return Some(m.clone());
        }

        self.default_model.clone()
    }
}

lazy_static! {
    static ref DB: HashMap<String, VendorProducts> = {
        let mut models: Vec<MouseModel> = Vec::new();
        for m in data::MODELS {
            models.push(MouseModel::new(m.0, m.1, m.2, m.3));
        }
        for m in data_import_from_chromiumos::MODELS {
            models.push(MouseModel::new(m.0, m.1, m.2, m.3));
        }

        let mut db: HashMap<String, VendorProducts> = HashMap::new();

        for m in models {
            let vendor_id = m.vendor_id.to_owned();
            match db.get_mut(&vendor_id) {
                Some(v) => {
                    v.add(m);
                }
                None => {
                    let mut v = VendorProducts::new();
                    v.add(m);
                    db.insert(vendor_id, v);
                }
            }
        }

        db
    };
}

/// "Standard" mouse CPI and polling rate.
const DEFAULT_MODEL: MouseModel = MouseModel {
    identifier: "default mouse",
    vendor_id: "*",
    product_id: "*",
    counts_per_mm: DEFAULT_COUNTS_PER_MM,
};

pub(crate) fn get_mouse_model(
    device_info: Option<fidl_input_report::DeviceInformation>,
) -> MouseModel {
    match device_info {
        None => DEFAULT_MODEL.clone(),
        Some(device_info) => {
            let vid = to_hex(device_info.vendor_id.unwrap_or_default());
            match DB.get(&vid) {
                Some(v) => match v.get(device_info.product_id.unwrap_or_default()) {
                    Some(m) => m,
                    None => DEFAULT_MODEL.clone(),
                },
                None => DEFAULT_MODEL.clone(),
            }
        }
    }
}

/// usb vendor_id and product_id are 16 bit int.
fn to_hex(id: u32) -> String {
    format!("{:04x}", id)
}

#[cfg(test)]
mod test {
    use super::super::{data, data_import_from_chromiumos};
    use super::*;
    use regex::Regex;
    use std::collections::HashSet;
    use test_case::test_case;

    #[test_case("*", "*", 1000, "default mouse" =>
      MouseModel {
        vendor_id: "*",
        identifier: "default mouse",
        product_id: "*",
        counts_per_mm: DEFAULT_COUNTS_PER_MM,
      }; "default mouse")]
    #[test_case("0001", "*", 1000, "any mouse of vendor" =>
      MouseModel {
        vendor_id: "0001",
        identifier: "any mouse of vendor",
        product_id: "*",
        counts_per_mm: DEFAULT_COUNTS_PER_MM,
      }; "any mouse of vendor")]
    #[test_case("0001", "001*", 1000, "pattern product_id" =>
      MouseModel {
        vendor_id: "0001",
        identifier: "pattern product_id",
        product_id: "001*",
        counts_per_mm: DEFAULT_COUNTS_PER_MM,
      }; "pattern product_id")]
    #[test_case("0001", "0002", 1000, "exact model" =>
      MouseModel {
        vendor_id: "0001",
        identifier: "exact model",
        product_id: "0002",
        counts_per_mm: DEFAULT_COUNTS_PER_MM,
      }; "exact model")]
    #[fuchsia::test]
    fn test_mouse_model_new(
        vendor_id: &'static str,
        product_id: &'static str,
        cpi: u32,
        identifier: &'static str,
    ) -> MouseModel {
        MouseModel::new(vendor_id, product_id, cpi, identifier)
    }

    #[test_case(0x046d, 0xc24c =>
      MouseModel::new("046d", "c24c", 4000, "Logitech G400s")
      ; "Known mouse")]
    #[test_case(0x046d, 0xc401 =>
      MouseModel::new("046d", "c40*", 600, "Logitech Trackballs*")
      ; "pattern match")]
    #[test_case(0x05ac, 0x0000 =>
      MouseModel::new("05ac", "*", 373, "Apple mice (other)")
      ; "any match")]
    #[test_case(0x046d, 0x0aaf =>
      MouseModel::new("*", "*", 1000, "default mouse")
      ; "Unknown device: this is a microphone")]
    #[fuchsia::test]
    fn test_get_mouse_model(vendor_id: u32, product_id: u32) -> MouseModel {
        get_mouse_model(Some(fidl_input_report::DeviceInformation {
            vendor_id: Some(vendor_id),
            product_id: Some(product_id),
            version: Some(0),
            polling_rate: Some(0),
            ..Default::default()
        }))
    }

    #[fuchsia::test]
    fn test_get_mouse_model_none() {
        pretty_assertions::assert_eq!(get_mouse_model(None), DEFAULT_MODEL);
    }

    #[fuchsia::test]
    fn no_duplicated_mouse_model() {
        let mut models: HashSet<(&'static str, &'static str)> = HashSet::new();
        let mut check_duplication =
            |filename: &'static str, list: &[(&'static str, &'static str, u32, &'static str)]| {
                for m in list {
                    let new_inserted = models.insert((m.0, m.1));
                    if !new_inserted {
                        panic!(
                            "found duplicated mouse model in {}: vendor: {}, product: {}",
                            filename, m.0, m.1
                        );
                    }
                }
            };

        check_duplication("data_import_from_chromiumos", &data_import_from_chromiumos::MODELS);
        check_duplication("data", &data::MODELS);
    }

    #[test_case(&data_import_from_chromiumos::MODELS; "data_import_from_chromiumos")]
    #[test_case(&data::MODELS; "data")]
    #[fuchsia::test]
    fn validate_vendor_id_product_id(models: &[(&'static str, &'static str, u32, &'static str)]) {
        let vendor_id_re = Regex::new(r"^[0-9a-f]{4}$").unwrap();
        let product_id_re = Regex::new(r"^[0-9a-f]{3}[0-9a-f\*]$").unwrap();
        for m in models {
            assert!(vendor_id_re.is_match(m.0), "vendor id should be 4 low case hex digit");
            if m.1 != "*" {
                assert!(
                    product_id_re.is_match(m.1),
                    r#"product id should be "* only" or "3 low case hex digit with ending *" or "4 low case hex digit""#
                );
            }
        }
    }
}
