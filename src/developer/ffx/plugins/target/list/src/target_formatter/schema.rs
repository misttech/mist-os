// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_validation::schema;

use super::JsonTarget;

schema! {
    type JsonTarget = struct {
        nodename: String,
        rcs_state: String,
        serial: String,
        target_type: String,
        target_state: String,
        addresses: Vec<String>,
        is_default: bool,
    };
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use crate::target_formatter::JsonTarget;

    fn examples() -> Vec<serde_json::Value> {
        vec![json!({
            "nodename": "target_name",
            "serial": "<unknown>",
            "addresses": [
                "127.0.0.1",
            ],
            "rcs_state": "Y",
            "target_type": "product.board",
            "target_state": "Product",
            "is_default": false,
        })]
    }

    #[test]
    fn test_json_target_validation() {
        ffx_validation::validation_test::<JsonTarget, JsonTarget>(&examples());
    }
}
