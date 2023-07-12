// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;

// GA 4 event constraints:
// Requests can have a maximum of 25 events.
// Events can have a maximum of 25 parameters.
// Events can have a maximum of 25 user properties.
// User property names must be 24 characters or fewer.
// User property values must be 36 characters or fewer.
// Event names must be 40 characters or fewer, may only contain alphanumeric characters and
//          underscores, and must start with an alphabetic character.
// Parameter names (including item parameters) must be 40 characters or fewer, may only contain
//          alphanumeric characters and underscores, and must start with an alphabetic character.
// Parameter values (including item parameter values) must be 100 characters or fewer.
// Item parameters can have a maximum of 10 custom parameters.
// The post body must be smaller than 130kB.

#[derive(Serialize, Deserialize, Debug)]
struct Event {
    name: String,
    params: Params,
}

#[derive(Serialize, Deserialize, Debug)]
struct Params {
    items: Vec<HashMap<String, String>>,
    #[serde(flatten)]
    params: HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct ValueObject {
    value: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct Post {
    client_id: String,
    user_id: String,
    non_personalized_ads: bool,
    user_properties: HashMap<String, ValueObject>,
    events: Vec<Event>,
}

#[cfg(test)]
mod tests {
    use crate::{Event, Params, Post, ValueObject, API_SECRET, DOMAIN, ENDPOINT, MEASUREMENT_ID};
    use reqwest::header::CONTENT_TYPE;
    use std::collections::HashMap;

    #[test]
    fn test_param_serde_flattening() {
        let item_map = HashMap::from([
            ("item1".to_string(), "value1".to_string()),
            ("item2".to_string(), "value2".to_string()),
        ]);
        let param_map = HashMap::from([
            ("param1".to_string(), "value1".to_string()),
            ("param2".to_string(), "value2".to_string()),
        ]);
        let p = Params { items: vec![item_map], params: param_map };
        let result = serde_json::json!(p);
        let expected_json = "{\"items\":[{\"item1\":\"value1\",\"item2\":\"value2\"}],\"param1\":\"value1\",\"param2\":\"value2\"}";
        assert_eq!(result.to_string(), expected_json);
    }
}
