// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_hwinfo::ProductInfo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
pub struct DeviceInfoImpl {
    product_info: HashMap<String, Option<String>>,
}

/// Store of "stable" Device information, like product name and serial.
/// Used as a base for data passed to Handlebars template renderer.
impl DeviceInfoImpl {
    pub fn new(product_info: Option<ProductInfo>) -> Self {
        let mut product_map = HashMap::new();
        match product_info {
            Some(info) => {
                println!("Product Information provided.");
                product_map.insert("SKU".to_string(), info.sku);
                product_map.insert("LANGUAGE".to_string(), info.language);
                product_map.insert("NAME".to_string(), info.name);
                product_map.insert("MODEL".to_string(), info.model);
                product_map.insert("MANUFACTURER".to_string(), info.manufacturer);
                product_map.insert("BUILD_DATE".to_string(), info.build_date);
                product_map.insert("BUILD_NAME".to_string(), info.build_name);
                product_map.insert("COLORWAY".to_string(), info.colorway);
                product_map.insert("DISPLAY".to_string(), info.display);
                product_map.insert("MEMORY".to_string(), info.memory);
                product_map.insert("NAND_STORAGE".to_string(), info.nand_storage);
                product_map.insert("EMMC_STORAGE".to_string(), info.emmc_storage);
                product_map.insert("MICROPHONE".to_string(), info.microphone);
                product_map.insert("AUDIO_AMPLIFIER".to_string(), info.audio_amplifier);
            }
            None => {
                println!("Product Information not provided");
            }
        }

        DeviceInfoImpl { product_info: product_map }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verifies placeholder implementation of DeviceInfo sets name/serial.
    fn no_product_values_when_not_provided() {
        let device = DeviceInfoImpl::new(None);
        assert_eq!(0, device.product_info.len());
    }

    #[test]
    fn product_values_present_when_provided() {
        let device = DeviceInfoImpl::new(Some(ProductInfo::EMPTY));

        // Gumshoe extracts 14 field from ProductInfo.
        assert_eq!(14, device.product_info.len());
    }
}
