// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::de::{self, Deserializer, Visitor};
use serde::ser::Serializer;
use std::fmt;
use url::Url;

pub fn serialize_url<S>(url: &Url, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&url.to_string())
}

pub fn deserialize_url<'de, D>(deserializer: D) -> Result<Url, D::Error>
where
    D: Deserializer<'de>,
{
    struct UrlVisitor;

    impl<'de> Visitor<'de> for UrlVisitor {
        type Value = Url;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("url")
        }

        fn visit_str<E>(self, url_str: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Url::parse(url_str).map_err(|err| {
                E::custom(format!("Failed to parse URL from string: {}: {}", url_str, err))
            })
        }
    }

    let visitor = UrlVisitor;
    deserializer.deserialize_str(visitor)
}
