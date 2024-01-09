// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde_json::{Number, Value};

use super::FieldId;

pub(crate) enum DiffSide<L, R = L> {
    Left(L),
    Right(R),
}

impl<L, R> DiffSide<L, R> {
    fn side_names(&self) -> &'static [&'static str; 2] {
        match self {
            Self::Left(_) => &["left", "right"],
            Self::Right(_) => &["right", "left"],
        }
    }
}

impl<L> DiffSide<L, L> {
    fn value(&self) -> &L {
        match self {
            Self::Left(v) | Self::Right(v) => v,
        }
    }
}

pub(crate) type KeyPair<'a> = (&'a str, &'a Value);
pub(crate) type DiffKeyPair<'l, 'r> = DiffSide<KeyPair<'l>, KeyPair<'r>>;
pub(crate) type Values<'l, 'r> = DiffSide<&'l [Value], &'r [Value]>;

pub(crate) trait DiffHandler<'l, 'r> {
    fn compare_next(&mut self, field: FieldId<'l>, lhs: &'l Value, rhs: &'r Value);

    fn type_mismatch(&mut self);
    fn bool_mismatch(&mut self, lhs: bool, rhs: bool);
    fn num_mismatch(&mut self, lhs: &'l Number, rhs: &'r Number);
    fn str_mismatch(&mut self, lhs: &'l str, rhs: &'r str);

    fn array_values_missing(&mut self, start: usize, values: Values<'l, 'r>);
    fn object_key_missing(&mut self, kv: DiffKeyPair<'l, 'r>);
}

/// Compare two JSON values, noting differences between them.
pub(crate) fn compare<'l, 'r>(h: &mut dyn DiffHandler<'l, 'r>, lhs: &'l Value, rhs: &'r Value) {
    if std::mem::discriminant(lhs) != std::mem::discriminant(rhs) {
        h.type_mismatch();
        return;
    }

    match (lhs, rhs) {
        (Value::Null, Value::Null) => {}
        (Value::Bool(lhs), Value::Bool(rhs)) if lhs != rhs => {
            h.bool_mismatch(*lhs, *rhs);
        }
        (Value::Number(lhs), Value::Number(rhs)) if lhs != rhs => h.num_mismatch(lhs, rhs),
        (Value::String(lhs), Value::String(rhs)) if lhs != rhs => h.str_mismatch(lhs, rhs),
        (Value::Array(lhs), Value::Array(rhs)) => {
            for (i, (lhs_val, rhs_val)) in lhs.iter().zip(rhs.iter()).enumerate() {
                h.compare_next(FieldId::Index(i as u32), lhs_val, rhs_val);
            }

            if lhs.len() > rhs.len() {
                h.array_values_missing(rhs.len(), Values::Left(&lhs[rhs.len()..]))
            } else if lhs.len() < rhs.len() {
                h.array_values_missing(lhs.len(), Values::Right(&rhs[lhs.len()..]))
            }
        }
        (Value::Object(lhs), Value::Object(rhs)) => {
            for (key, val) in lhs.iter() {
                let key = &**key;

                if let Some(rhs_val) = rhs.get(key) {
                    h.compare_next(key.into(), val, rhs_val);
                    continue;
                }

                h.object_key_missing(DiffKeyPair::Left((key, val)));
            }

            for (key, val) in rhs.iter() {
                let key = &**key;

                if lhs.contains_key(key) {
                    continue;
                }

                h.object_key_missing(DiffKeyPair::Right((key, val)));
            }
        }
        _ => {}
    }
}

struct Path<'a> {
    field: FieldId<'a>,
    prev: Option<&'a Path<'a>>,
}

impl<'a> std::fmt::Display for Path<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(prev) = self.prev {
            prev.fmt(f)?;
        }
        self.field.fmt(f)
    }
}

pub(crate) struct ValidationTestDiff<'l, 'r, 'p> {
    lhs: &'l Value,
    rhs: &'r Value,
    found_difference: bool,
    path: Option<&'p Path<'l>>,
}

impl<'l: 'p, 'r: 'p, 'p> ValidationTestDiff<'l, 'r, 'p> {
    pub(crate) fn check(lhs: &'l Value, rhs: &'r Value) -> Result<(), ()> {
        let mut this = Self { lhs, rhs, found_difference: false, path: None };
        compare(&mut this, lhs, rhs);
        (!this.found_difference).then_some(()).ok_or(())
    }

    fn error<'a>(&mut self, f: std::fmt::Arguments<'a>) {
        if let Some(p) = self.path {
            eprint!("{p}: ");
        } else {
            eprint!("#root: ");
        }
        eprintln!("{f}");
        self.found_difference = true;
    }
}

impl<'l: 'p, 'r: 'p, 'p> DiffHandler<'l, 'r> for ValidationTestDiff<'l, 'r, 'p> {
    fn compare_next(&mut self, field: FieldId<'l>, lhs: &'l Value, rhs: &'r Value) {
        let path = &Path { field, prev: self.path };
        let mut this = ValidationTestDiff { lhs, rhs, found_difference: false, path: Some(path) };
        compare(&mut this, lhs, rhs);
        self.found_difference |= this.found_difference;
    }

    fn type_mismatch(&mut self) {
        let lhs = self.lhs;
        let rhs = self.rhs;
        self.error(format_args!("type mismatch between {} and {}", lhs, rhs));
    }

    fn bool_mismatch(&mut self, lhs: bool, rhs: bool) {
        self.error(format_args!("{lhs} != {rhs}"));
    }

    fn num_mismatch(&mut self, lhs: &'l Number, rhs: &'r Number) {
        self.error(format_args!("{lhs} != {rhs}"));
    }

    fn str_mismatch(&mut self, lhs: &'l str, rhs: &'r str) {
        self.error(format_args!("{lhs:?} != {rhs:?}"));
    }

    fn array_values_missing(&mut self, _start: usize, values: Values<'l, 'r>) {
        let [side, other_side] = values.side_names();
        let values = Value::from(*values.value());
        self.error(format_args!(
            "missing array values on {other_side} side; {side} side values: {values}"
        ));
    }

    fn object_key_missing(&mut self, kv: DiffKeyPair<'l, 'r>) {
        let [side, other_side] = kv.side_names();
        let (key, value) = kv.value();
        self.error(format_args!(
            "missing key {key:?} on {other_side} side; {side} side value: {value}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn example_json() -> Value {
        json!({
            "a": 123,
            "b": false,
            "c": "string",
            "d": [456.0, true, "", [], {}],
            "e": null,
            "f": {"a": 12345},
        })
    }

    fn different_example_json() -> Value {
        json!({
            "a": 1234,
            "b": true,
            "c": "another string",
            "d": [true, "", [], {}],
            "f": {},
        })
    }

    #[test]
    fn test_same_json_is_ok() {
        let value = example_json();
        ValidationTestDiff::check(&value, &value).expect("test should succeed");
    }

    #[test]
    fn test_different_json_fails() {
        ValidationTestDiff::check(&example_json(), &different_example_json())
            .expect_err("test should fail");
    }
}
