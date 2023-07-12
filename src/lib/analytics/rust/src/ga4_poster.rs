// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use once::assert_has_not_been_called;

use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;

const MEASUREMENT_ID: &str = "G-5DDQ7RYF6M";
const API_SECRET: &str = "J9BlGsivS5a3a-Synb-n1Q";
const DOMAIN: &str = "www.google-analytics.com";
const ENDPOINT: &str = "/mp/collect";

#[tokio::main]
async fn main() {
    init();
}

fn init() {
    assert_has_not_been_called!("the init function must only be called {}", "once");
}

#[cfg(test)]
mod tests {
    use crate::{Event, Params, Post, ValueObject, API_SECRET, DOMAIN, ENDPOINT, MEASUREMENT_ID};
    use reqwest::header::CONTENT_TYPE;
    use std::collections::HashMap;

    #[test]
    fn test_post_with_two_events() {
        let event_name = String::from("video_resume");
        let event_params = [
            ("category".to_string(), "video".to_string()),
            ("label".to_string(), "holiday".to_string()),
            ("value".to_string(), "300".to_string()),
        ];
        let params =
            Params { items: vec![HashMap::from([])], params: event_params.into_iter().collect() };
        let event = Event { name: event_name, params };

        let event_params2 = HashMap::from([
            ("category".to_string(), "video".to_string()),
            ("label".to_string(), "surf".to_string()),
        ]);

        let params2 =
            Params { items: vec![HashMap::from([])], params: event_params2.into_iter().collect() };
        let event2 = Event { name: "video_pause".to_string(), params: params2 };
        let events = vec![event, event2];

        let user_props: HashMap<String, ValueObject> = HashMap::new();
        let client_id = "1";
        let event_name = String::from("video_resume");
        let event_params = [
            ("category".to_string(), "video".to_string()),
            ("label".to_string(), "holiday".to_string()),
            ("value".to_string(), "300".to_string()),
        ];
        let params =
            Params { items: vec![HashMap::from([])], params: event_params.into_iter().collect() };
        let event = Event { name: event_name, params };

        let event_params2 = HashMap::from([
            ("category".to_string(), "video".to_string()),
            ("label".to_string(), "surf".to_string()),
        ]);

        let params2 =
            Params { items: vec![HashMap::from([])], params: event_params2.into_iter().collect() };
        let event2 = Event { name: "video_pause".to_string(), params: params2 };
        let events = vec![event, event2];

        let user_props = HashMap::new();
        let client_id = "1";
        let post = Post {
            client_id: client_id.to_string(),
            user_id: "1".to_string(),
            non_personalized_ads: true,
            events,
            user_properties: user_props,
        };

        let body = serde_json::json!(post);
        println!("BODY = {}", body);

        let client = reqwest::Client::new();
        let url = format!(
            "https://{}{}?api_secret={}&measurement_id={}",
            DOMAIN, ENDPOINT, API_SECRET, MEASUREMENT_ID
        );
        println!("URL = {}", url);
        tokio_test::block_on(async {
            let response2 = client
                .post(&url)
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .send()
                .await;

            match response2 {
                Ok(res) => match res.status() {
                    reqwest::StatusCode::OK | reqwest::StatusCode::NO_CONTENT => {
                        println!("Success {:?}", res);
                    }
                    _ => {
                        println!("Uhoh {:?}", res);
                    }
                },
                Err(e) => println!("Error {:?}", e),
            };
        });
    }
}
