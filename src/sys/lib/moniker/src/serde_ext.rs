// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        child_moniker::{ChildMoniker, ChildMonikerBase},
        moniker::{Moniker, MonikerBase},
    },
    serde::{
        de::{self, Deserializer, Visitor},
        Deserialize, Serialize, Serializer,
    },
    std::fmt,
};

impl Serialize for ChildMoniker {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

struct ChildMonikerVisitor;

impl<'de> Visitor<'de> for ChildMonikerVisitor {
    type Value = ChildMoniker;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a child moniker of a component instance")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match ChildMoniker::parse(value) {
            Ok(moniker) => Ok(moniker),
            Err(err) => Err(E::custom(format!("Failed to parse ChildMoniker: {}", err))),
        }
    }
}

impl<'de> Deserialize<'de> for ChildMoniker {
    fn deserialize<D>(deserializer: D) -> Result<ChildMoniker, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(ChildMonikerVisitor)
    }
}

impl Serialize for Moniker {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

struct MonikerVisitor;

impl<'de> Visitor<'de> for MonikerVisitor {
    type Value = Moniker;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an absolute moniker of a component instance")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match Moniker::parse_str(value) {
            Ok(moniker) => Ok(moniker),
            Err(err) => Err(E::custom(format!("Failed to parse Moniker: {}", err))),
        }
    }
}

impl<'de> Deserialize<'de> for Moniker {
    fn deserialize<D>(deserializer: D) -> Result<Moniker, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(MonikerVisitor)
    }
}
