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
