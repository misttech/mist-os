// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{any::TypeId, borrow::Cow, collections::HashMap, fmt::Display, ops::ControlFlow};

use crate::schema::*;

/// Test-only. Validates that the given JSON value follows the schema.
pub fn validate(schema: Walk, value: &serde_json::Value) -> Result<(), Vec<ValidationError>> {
    let mut validation = Validation::new(value);

    if schema(&mut validation).is_continue() {
        eprintln!("Validation failed:");
        for error in &validation.error {
            eprintln!("{error}");
        }
        Err(validation.error)
    } else {
        Ok(())
    }
}

/// Represents either a named struct field, or an indexed tuple struct field.
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum FieldId {
    Name(Cow<'static, str>),
    // Tuple struct/enum fields
    Index(u32),
}

impl Display for FieldId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name(key) => write!(f, ".{key}"),
            Self::Index(i) => write!(f, "[{i}]"),
        }
    }
}

impl From<&'static str> for FieldId {
    fn from(value: &'static str) -> Self {
        Self::Name(Cow::Borrowed(value))
    }
}

impl From<u32> for FieldId {
    fn from(value: u32) -> Self {
        Self::Index(value)
    }
}

/// Error messages presented to the developer when example validation fails.
#[derive(thiserror::Error, Clone, PartialEq, Eq, Debug)]
pub enum ValidationErrorMessage {
    #[error("Expected {expected:?} got {actual}")]
    TypeMismatch { expected: Cow<'static, [ValueType]>, actual: ValueType },
    #[error("Expected {len}-element array, got {actual} ({actual_len:?} elements)")]
    TypeMismatchFixedArray { len: usize, actual: ValueType, actual_len: Option<usize> },
    #[error("Unexpected field in strict struct")]
    UnexpectedField,
    #[error("Missing required field")]
    MissingField,
    #[error("Expected enum data for variant {0}")]
    EnumExpectedData(&'static str),
    #[error("Unexpected enum data for variant {0}")]
    EnumUnexpectedData(&'static str),
    #[error("Unknown enum variant")]
    UnknownEnumVariant,
    #[error("Expected value {0}")]
    ConstantMismatch(serde_json::Value),
}

/// A collection of validation error messages for a value and any subfields.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct ValidationError {
    messages: Vec<ValidationErrorMessage>,
    fields: HashMap<FieldId, Vec<ValidationError>>,
}

impl Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut stack = vec![(self, None, None)];
        while let Some((this, outer_field, mut field_iter)) = stack.pop() {
            for message in &this.messages {
                writeln!(f, "{0:>1$}{message}", "", stack.len() * 2)?;
            }

            let iter = field_iter.get_or_insert(
                this.fields.iter().flat_map(|(key, errs)| errs.iter().map(move |err| (key, err))),
            );

            if let Some((key, err)) = iter.next() {
                writeln!(f, "{0:>1$}{key}:", "", stack.len() * 2)?;
                stack.push((this, outer_field, field_iter));
                stack.push((err, Some(key), None));
            }
        }
        Ok(())
    }
}

impl ValidationError {
    fn message(message: ValidationErrorMessage) -> Self {
        Self { messages: vec![message], ..Self::default() }
    }

    fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.fields.is_empty()
    }

    fn push_field(&mut self, field: FieldId, errors: impl IntoIterator<Item = ValidationError>) {
        use std::collections::hash_map::Entry;
        let entry = self.fields.entry(field);

        match entry {
            Entry::Occupied(mut entry) => {
                entry.get_mut().extend(errors);
            }
            Entry::Vacant(entry) => {
                let errors: Vec<_> = errors.into_iter().collect();
                if errors.is_empty() {
                    return;
                }
                entry.insert(errors);
            }
        }
    }
}

impl Into<ValidationError> for ValidationErrorMessage {
    fn into(self) -> ValidationError {
        ValidationError { messages: vec![self], ..ValidationError::default() }
    }
}

/// Validates a schema against a JSON value and accumulates structured errors describing where
/// and how validation failed.
struct Validation<'a> {
    value: &'a serde_json::Value,
    error: Vec<ValidationError>,
}

impl<'a> Validation<'a> {
    fn new(value: &'a serde_json::Value) -> Self {
        Self { value, error: vec![] }
    }

    /// Stop accepting types since one has matched.
    fn success(&mut self) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }

    /// Continue accepting new types until one matches.
    fn fail(&mut self) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Continue(self)
    }

    fn fail_with_err(
        &mut self,
        error: impl Into<ValidationError>,
    ) -> ControlFlow<(), &mut dyn Walker> {
        self.error.push(error.into());
        self.fail()
    }
}

impl<'a> Walker for Validation<'a> {
    fn add_alias(
        &mut self,
        _name: &'static str,
        _id: TypeId,
        ty: Walk,
    ) -> ControlFlow<(), &mut dyn Walker> {
        // TODO: Utilize all known aliases for the given TypeId
        ty(self)?;
        self.fail()
    }

    fn add_struct(
        &mut self,
        fields: &'static [Field],
        extras: Option<StructExtras>,
    ) -> ControlFlow<(), &mut dyn Walker> {
        let Some(object) = self.value.as_object() else {
            return self.fail_with_err(ValidationErrorMessage::TypeMismatch {
                expected: Cow::Borrowed(&[ValueType::Object]),
                actual: self.value.into(),
            });
        };

        let mut error = ValidationError::default();

        match &extras {
            Some(StructExtras::Deny) => {
                for key in object.keys() {
                    if fields.iter().position(|f| f.key == key).is_none() {
                        error.push_field(
                            FieldId::Name(key.clone().into()),
                            [ValidationError::message(ValidationErrorMessage::UnexpectedField)],
                        );
                    }
                }
            }
            None | Some(StructExtras::Flatten(..)) => {}
        }

        for field in fields {
            let Some(value) = object.get(field.key) else {
                if !field.optional {
                    error.push_field(
                        FieldId::Name(field.key.into()),
                        [ValidationError::message(ValidationErrorMessage::MissingField)],
                    );
                }
                continue;
            };

            // Run validation on the field value
            let mut validation = Validation::new(value);

            if (field.value)(&mut validation).is_continue() {
                // Validation failed
                error.push_field(FieldId::Name(Cow::Borrowed(field.key)), validation.error);
            }
        }

        if !error.is_empty() {
            self.fail_with_err(error)
        } else {
            self.success()
        }
    }

    fn add_enum(
        &mut self,
        variants: &'static [(&'static str, Walk)],
    ) -> ControlFlow<(), &mut dyn Walker> {
        match self.value {
            serde_json::Value::String(s) => {
                for (name, value) in variants {
                    if name == s {
                        // Verify that the enum does not expect a value.
                        if value(&mut Never).is_continue() {
                            return self.success();
                        }

                        return self.fail_with_err(ValidationErrorMessage::EnumExpectedData(*name));
                    }
                }
            }
            serde_json::Value::Object(object) => {
                for (name, value) in variants {
                    if let Some(variant_value) = object.get(*name) {
                        // Verify that the enum expects some data.
                        if value(&mut Never).is_continue() {
                            return self
                                .fail_with_err(ValidationErrorMessage::EnumUnexpectedData(*name));
                        }

                        let mut validation = Validation::new(variant_value);
                        value(&mut validation)?;

                        let mut error = ValidationError::default();
                        error.push_field(FieldId::Name(Cow::Borrowed(*name)), validation.error);
                        return self.fail_with_err(error);
                    }
                }
            }
            _ => {
                return self.fail_with_err(ValidationErrorMessage::TypeMismatch {
                    expected: Cow::Borrowed(&[ValueType::Object, ValueType::String]),
                    actual: self.value.into(),
                })
            }
        }

        self.fail_with_err(ValidationErrorMessage::UnknownEnumVariant)
    }

    fn add_tuple(&mut self, fields: &'static [Walk]) -> ControlFlow<(), &mut dyn Walker> {
        let Some(array) = self.value.as_array().filter(|a| a.len() == fields.len()) else {
            return self.fail_with_err(ValidationErrorMessage::TypeMismatchFixedArray {
                len: fields.len(),
                actual: self.value.into(),
                actual_len: self.value.as_array().map(|a| a.len()),
            });
        };

        let mut error = ValidationError::default();

        for (i, (field, value)) in fields.iter().zip(array.iter()).enumerate() {
            let mut validation = Validation::new(value);

            if field(&mut validation).is_continue() {
                error.push_field(FieldId::Index(i as u32), validation.error);
            }
        }

        if !error.is_empty() {
            self.fail_with_err(error)
        } else {
            self.success()
        }
    }

    fn add_array(&mut self, size: Option<usize>, ty: Walk) -> ControlFlow<(), &mut dyn Walker> {
        let Some(array) = self.value.as_array().filter(|a| size.is_none() || Some(a.len()) == size)
        else {
            return self.fail_with_err(if let Some(size) = size {
                ValidationErrorMessage::TypeMismatchFixedArray {
                    len: size,
                    actual: self.value.into(),
                    actual_len: self.value.as_array().map(|a| a.len()),
                }
            } else {
                ValidationErrorMessage::TypeMismatch {
                    expected: Cow::Borrowed(&[ValueType::Array]),
                    actual: self.value.into(),
                }
            });
        };

        let mut error = ValidationError::default();

        for (i, value) in array.iter().enumerate() {
            let mut validation = Validation::new(value);

            if ty(&mut validation).is_continue() {
                error.push_field(FieldId::Index(i as u32), validation.error);
            }
        }

        if !error.is_empty() {
            self.fail_with_err(error)
        } else {
            self.success()
        }
    }

    fn add_map(&mut self, _key: Walk, _value: Walk) -> ControlFlow<(), &mut dyn Walker> {
        todo!("Validation not yet implemented - b/315385828")
    }

    fn add_type(&mut self, ty: ValueType) -> ControlFlow<(), &mut dyn Walker> {
        let mut value_ty = ValueType::from(self.value);

        if ty == ValueType::Double && value_ty == ValueType::Integer {
            // Promote integer to double
            value_ty = ty;
        }

        if ty != value_ty {
            self.fail_with_err(ValidationErrorMessage::TypeMismatch {
                expected: Cow::Owned(vec![ty]),
                actual: self.value.into(),
            })
        } else {
            self.success()
        }
    }

    fn add_any(&mut self) -> ControlFlow<(), &mut dyn Walker> {
        self.success()
    }

    fn add_constant(&mut self, value: serde_json::Value) -> ControlFlow<(), &mut dyn Walker> {
        if self.value != &value {
            self.fail_with_err(ValidationErrorMessage::ConstantMismatch(value))
        } else {
            self.success()
        }
    }
}

/// Expects that a schema is empty.
struct Never;

impl Walker for Never {
    fn add_alias(
        &mut self,
        _name: &'static str,
        _id: TypeId,
        _ty: Walk,
    ) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }

    fn add_struct(
        &mut self,
        _fields: &'static [Field],
        _extras: Option<StructExtras>,
    ) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }

    fn add_enum(
        &mut self,
        _variants: &'static [(&'static str, Walk)],
    ) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }

    fn add_tuple(&mut self, _fields: &'static [Walk]) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }

    fn add_array(&mut self, _size: Option<usize>, _ty: Walk) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }

    fn add_map(&mut self, _key: Walk, _value: Walk) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }

    fn add_type(&mut self, _ty: ValueType) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }

    fn add_any(&mut self) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }

    fn add_constant(&mut self, _value: serde_json::Value) -> ControlFlow<(), &mut dyn Walker> {
        ControlFlow::Break(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{field, schema::json};

    use super::*;

    #[test]
    fn test_basic_json_types() {
        validate(json::Null::walk_schema, &json!(null)).unwrap();
        validate(json::Bool::walk_schema, &json!(true)).unwrap();
        validate(json::Integer::walk_schema, &json!(123)).unwrap();
        validate(json::Double::walk_schema, &json!(123)).unwrap();
        validate(json::Double::walk_schema, &json!(123.4)).unwrap();
        validate(json::String::walk_schema, &json!("hello!")).unwrap();
        validate(json::Array::walk_schema, &json!([1, 2, 3])).unwrap();
        validate(json::Array::walk_schema, &json!([])).unwrap();
        validate(json::Object::walk_schema, &json!({"hello": "world"})).unwrap();
        validate(json::Object::walk_schema, &json!({})).unwrap();

        validate(json::Any::walk_schema, &json!(null)).unwrap();
        validate(json::Any::walk_schema, &json!(true)).unwrap();
        validate(json::Any::walk_schema, &json!(123)).unwrap();
        validate(json::Any::walk_schema, &json!(123)).unwrap();
        validate(json::Any::walk_schema, &json!(123.4)).unwrap();
        validate(json::Any::walk_schema, &json!("hello!")).unwrap();
        validate(json::Any::walk_schema, &json!([1, 2, 3])).unwrap();
        validate(json::Any::walk_schema, &json!([])).unwrap();
        validate(json::Any::walk_schema, &json!({"hello": "world"})).unwrap();
        validate(json::Any::walk_schema, &json!({})).unwrap();
    }

    #[test]
    fn test_std_types() {
        let ints = [
            u8::walk_schema,
            i8::walk_schema,
            u16::walk_schema,
            i16::walk_schema,
            u32::walk_schema,
            i32::walk_schema,
            u64::walk_schema,
            i64::walk_schema,
        ];
        for int in ints {
            validate(int, &json!(0)).unwrap();
        }

        validate(f32::walk_schema, &json!(0)).unwrap();
        validate(f32::walk_schema, &json!(0.5)).unwrap();
        validate(f64::walk_schema, &json!(0)).unwrap();
        validate(f64::walk_schema, &json!(0.5)).unwrap();

        validate(bool::walk_schema, &json!(false)).unwrap();

        validate(str::walk_schema, &json!("hello")).unwrap();
        validate(String::walk_schema, &json!("hello")).unwrap();

        validate(Option::<bool>::walk_schema, &json!(null)).unwrap();
        validate(Option::<bool>::walk_schema, &json!(true)).unwrap();

        validate(Box::<i32>::walk_schema, &json!(123)).unwrap();

        validate(Vec::<i32>::walk_schema, &json!([1, 2, 3, 4])).unwrap();
        validate(<[i32]>::walk_schema, &json!([1, 2, 3, 4])).unwrap();
        validate(<[i32; 4]>::walk_schema, &json!([1, 2, 3, 4])).unwrap();
        validate(<[i32; 3]>::walk_schema, &json!([1, 2, 3, 4])).unwrap_err();
        validate(<[i32; 5]>::walk_schema, &json!([1, 2, 3, 4])).unwrap_err();

        validate(<()>::walk_schema, &json!(null)).unwrap();

        validate(<(u32,)>::walk_schema, &json!([123])).unwrap();
        validate(<(u32, bool)>::walk_schema, &json!([123, false])).unwrap();
        validate(<(u32, bool, String)>::walk_schema, &json!([123, true, "Hello!"])).unwrap();
        validate(<(u32, bool, String, ())>::walk_schema, &json!([123, true, "Hello!", null]))
            .unwrap();
    }

    fn walk_struct(walker: &mut dyn Walker) -> ControlFlow<()> {
        walker.add_struct(&[field!(b: bool), field!(n: u32), field!(s: String)], None)?.ok()
    }

    #[test]
    fn test_struct_ok() {
        validate(
            walk_struct,
            &json!({
                "b": true,
                "n": 1234,
                "s": "String",
            }),
        )
        .expect("Validation failed");
    }

    #[test]
    fn test_struct_type_mismatch() {
        assert_eq!(
            validate(walk_struct, &json!([1234])),
            Err(vec![ValidationError {
                messages: vec![ValidationErrorMessage::TypeMismatch {
                    expected: Cow::Borrowed(&[ValueType::Object]),
                    actual: ValueType::Array,
                }],
                fields: HashMap::new(),
            }])
        );

        assert_eq!(
            validate(walk_struct, &json!(true)),
            Err(vec![ValidationError {
                messages: vec![ValidationErrorMessage::TypeMismatch {
                    expected: Cow::Borrowed(&[ValueType::Object]),
                    actual: ValueType::Bool,
                }],
                fields: HashMap::new(),
            }])
        );

        assert_eq!(
            validate(walk_struct, &json!("string")),
            Err(vec![ValidationError {
                messages: vec![ValidationErrorMessage::TypeMismatch {
                    expected: Cow::Borrowed(&[ValueType::Object]),
                    actual: ValueType::String,
                }],
                fields: HashMap::new(),
            }])
        );
    }

    #[test]
    fn test_struct_field_type_mismatch() {
        let errs = validate(
            walk_struct,
            &json!({
                "b": 1234,
                "n": false,
                "s": [],
            }),
        )
        .expect_err("Validation succeeded");

        assert!(errs.len() == 1);

        let err = &errs[0];
        let expected = ValidationError {
            messages: vec![],
            fields: HashMap::from_iter([
                (
                    FieldId::from("b"),
                    vec![ValidationErrorMessage::TypeMismatch {
                        expected: Cow::Borrowed(&[ValueType::Bool]),
                        actual: ValueType::Integer,
                    }
                    .into()],
                ),
                (
                    FieldId::from("n"),
                    vec![ValidationErrorMessage::TypeMismatch {
                        expected: Cow::Borrowed(&[ValueType::Integer]),
                        actual: ValueType::Bool,
                    }
                    .into()],
                ),
                (
                    FieldId::from("s"),
                    vec![ValidationErrorMessage::TypeMismatch {
                        expected: Cow::Borrowed(&[ValueType::String]),
                        actual: ValueType::Array,
                    }
                    .into()],
                ),
            ]),
        };

        assert_eq!(err, &expected);
    }
}
