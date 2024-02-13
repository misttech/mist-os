// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// An alternative to [serde_json::Value] that is usable in const contexts and bump allocators.
/// Serde's struct owns heap-allocated types like `String`, `Vec`, and `HashMap` which must
/// be dropped to deallocate their memory.
pub enum InlineValue<'a> {
    Null,
    Bool(bool),
    UInt(u64),
    Int(i64),
    Float(f64),
    String(&'a str),
    Array(&'a [&'a InlineValue<'a>]),
    Object(&'a [(&'a str, &'a InlineValue<'a>)]),
}

impl<'a> std::fmt::Debug for InlineValue<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(arg0) => arg0.fmt(f),
            Self::UInt(arg0) => arg0.fmt(f),
            Self::Int(arg0) => arg0.fmt(f),
            Self::Float(arg0) => arg0.fmt(f),
            Self::String(arg0) => arg0.fmt(f),
            Self::Array(arg0) => arg0.fmt(f),
            Self::Object(arg0) => f.debug_map().entries(arg0.iter().copied()).finish(),
        }
    }
}

impl<'a> InlineValue<'a> {
    /// Compares this inline value to the given [serde_json::Value].
    pub fn matches_json(&self, json: &serde_json::Value) -> bool {
        use serde_json::Value;
        match (self, json) {
            (Self::Null, Value::Null) => true,
            (Self::Bool(a), Value::Bool(b)) => a == b,
            (Self::UInt(a), Value::Number(b)) => Some(*a) == b.as_u64(),
            (Self::Int(a), Value::Number(b)) => Some(*a) == b.as_i64(),
            (Self::Float(a), Value::Number(b)) => Some(*a) == b.as_f64(),
            (Self::String(a), Value::String(b)) => a == b,
            (Self::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| a.matches_json(b))
            }
            (Self::Object(a), Value::Object(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(name, a)| b.get(*name).is_some_and(|b| a.matches_json(b)))
            }
            _ => false,
        }
    }
}

impl<'a> Into<serde_json::Value> for &'_ InlineValue<'a> {
    fn into(self) -> serde_json::Value {
        use serde_json::Value;
        match *self {
            InlineValue::Null => Value::Null,
            InlineValue::Bool(v) => v.into(),
            InlineValue::UInt(v) => v.into(),
            InlineValue::Int(v) => v.into(),
            InlineValue::Float(v) => v.into(),
            InlineValue::String(v) => v.into(),
            InlineValue::Array(v) => Value::Array(v.iter().copied().map(Into::into).collect()),
            InlineValue::Object(v) => {
                Value::Object(v.iter().copied().map(|(k, v)| (k.into(), v.into())).collect())
            }
        }
    }
}
