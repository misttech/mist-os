// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{any::TypeId, borrow::Cow, collections::HashMap, fmt::Display, ops::ControlFlow};

use crate::schema::*;

pub(crate) mod compare;

/// Test-only. Validates that the given JSON value follows the schema.
pub fn validate<'a>(
    schema: &'a Type<'a>,
    value: &serde_json::Value,
) -> Result<(), Vec<ValidationError<'a>>> {
    let mut validation = Validation::new(value);

    if schema.walk(&mut validation).is_continue() {
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
pub enum FieldId<'a> {
    Name(Cow<'a, str>),
    // Tuple struct/enum fields
    Index(u32),
}

impl<'a> Display for FieldId<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name(key) => write!(f, ".{key}"),
            Self::Index(i) => write!(f, "[{i}]"),
        }
    }
}

impl<'a> From<&'a str> for FieldId<'a> {
    fn from(value: &'a str) -> Self {
        Self::Name(Cow::Borrowed(value))
    }
}

impl From<u32> for FieldId<'static> {
    fn from(value: u32) -> Self {
        Self::Index(value)
    }
}

/// Error messages presented to the developer when example validation fails.
#[derive(thiserror::Error, Clone, PartialEq, Eq, Debug)]
pub enum ValidationErrorMessage<'a> {
    #[error("Expected {expected:?} got {actual}")]
    TypeMismatch { expected: Cow<'a, [ValueType]>, actual: ValueType },
    #[error("Expected {len}-element array, got {actual} ({actual_len:?} elements)")]
    TypeMismatchFixedArray { len: usize, actual: ValueType, actual_len: Option<usize> },
    #[error("Unexpected field in strict struct")]
    UnexpectedField,
    #[error("Missing required field")]
    MissingField,
    #[error("Expected enum data for variant {0}")]
    EnumExpectedData(&'a str),
    #[error("Unexpected enum data for variant {0}")]
    EnumUnexpectedData(&'a str),
    #[error("Unknown enum variant")]
    UnknownEnumVariant,
    #[error("Expected value {0}")]
    ConstantMismatch(serde_json::Value),
}

/// A collection of validation error messages for a value and any subfields.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct ValidationError<'a> {
    messages: Vec<ValidationErrorMessage<'a>>,
    fields: HashMap<FieldId<'a>, Vec<ValidationError<'a>>>,
}

impl<'a> Display for ValidationError<'a> {
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

impl<'a> ValidationError<'a> {
    fn message(message: ValidationErrorMessage<'a>) -> Self {
        Self { messages: vec![message], ..Self::default() }
    }

    fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.fields.is_empty()
    }

    fn push_field(
        &mut self,
        field: FieldId<'a>,
        errors: impl IntoIterator<Item = ValidationError<'a>>,
    ) {
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

impl<'a> Into<ValidationError<'a>> for ValidationErrorMessage<'a> {
    fn into(self) -> ValidationError<'a> {
        ValidationError { messages: vec![self], ..ValidationError::default() }
    }
}

/// Validates a schema against a JSON value and accumulates structured errors describing where
/// and how validation failed.
struct Validation<'j, 's> {
    value: &'j serde_json::Value,
    error: Vec<ValidationError<'s>>,
}

impl<'j, 's> Validation<'j, 's> {
    fn new(value: &'j serde_json::Value) -> Self {
        Self { value, error: vec![] }
    }

    /// Stop accepting types since one has matched.
    fn success(&mut self) -> ControlFlow<()> {
        ControlFlow::Break(())
    }

    /// Continue accepting new types until one matches.
    fn fail(&mut self) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    fn fail_with_err(&mut self, error: impl Into<ValidationError<'s>>) -> ControlFlow<()> {
        self.error.push(error.into());
        self.fail()
    }
}

impl<'j, 's> Walker<'s> for Validation<'j, 's> {
    fn add_alias(&mut self, _name: &'s str, _id: TypeId, ty: &'s Type<'s>) -> ControlFlow<()> {
        // TODO: Utilize all known aliases for the given TypeId
        ty.walk(self)?;
        self.fail()
    }

    fn add_struct(
        &mut self,
        fields: &'s [Field<'s>],
        extras: Option<StructExtras<'s>>,
    ) -> ControlFlow<()> {
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

            if field.value.walk(&mut validation).is_continue() {
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

    fn add_enum(&mut self, variants: &'s [(&'s str, &'s Type<'s>)]) -> ControlFlow<()> {
        match self.value {
            serde_json::Value::String(s) => {
                for (name, value) in variants {
                    if name == s {
                        // Verify that the enum does not expect a value.
                        if value.walk(&mut Never).is_continue() {
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
                        if value.walk(&mut Never).is_continue() {
                            return self
                                .fail_with_err(ValidationErrorMessage::EnumUnexpectedData(*name));
                        }

                        let mut validation = Validation::new(variant_value);
                        value.walk(&mut validation)?;

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

    fn add_tuple(&mut self, fields: &'s [&'s Type<'s>]) -> ControlFlow<()> {
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

            if field.walk(&mut validation).is_continue() {
                error.push_field(FieldId::Index(i as u32), validation.error);
            }
        }

        if !error.is_empty() {
            self.fail_with_err(error)
        } else {
            self.success()
        }
    }

    fn add_array(&mut self, size: Option<usize>, ty: &'s Type<'s>) -> ControlFlow<()> {
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

            if ty.walk(&mut validation).is_continue() {
                error.push_field(FieldId::Index(i as u32), validation.error);
            }
        }

        if !error.is_empty() {
            self.fail_with_err(error)
        } else {
            self.success()
        }
    }

    fn add_map(&mut self, _key: &'s Type<'s>, _value: &'s Type<'s>) -> ControlFlow<()> {
        todo!("Validation not yet implemented - b/315385828")
    }

    fn add_type(&mut self, ty: ValueType) -> ControlFlow<()> {
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

    fn add_any(&mut self) -> ControlFlow<()> {
        self.success()
    }

    fn add_constant(&mut self, value: &'s InlineValue<'s>) -> ControlFlow<()> {
        if !value.matches_json(self.value) {
            self.fail_with_err(ValidationErrorMessage::ConstantMismatch(value.into()))
        } else {
            self.success()
        }
    }
}

/// Expects that a schema is empty.
struct Never;

impl<'a> Walker<'a> for Never {
    fn add_alias(&mut self, _name: &'a str, _id: TypeId, _ty: &'a Type<'a>) -> ControlFlow<()> {
        ControlFlow::Break(())
    }

    fn add_struct(
        &mut self,
        _fields: &'a [Field<'a>],
        _extras: Option<StructExtras<'a>>,
    ) -> ControlFlow<()> {
        ControlFlow::Break(())
    }

    fn add_enum(&mut self, _variants: &'a [(&'a str, &'a Type<'a>)]) -> ControlFlow<()> {
        ControlFlow::Break(())
    }

    fn add_tuple(&mut self, _fields: &'a [&'a Type<'a>]) -> ControlFlow<()> {
        ControlFlow::Break(())
    }

    fn add_array(&mut self, _size: Option<usize>, _ty: &'a Type<'a>) -> ControlFlow<()> {
        ControlFlow::Break(())
    }

    fn add_map(&mut self, _key: &'a Type<'a>, _value: &'a Type<'a>) -> ControlFlow<()> {
        ControlFlow::Break(())
    }

    fn add_type(&mut self, _ty: ValueType) -> ControlFlow<()> {
        ControlFlow::Break(())
    }

    fn add_any(&mut self) -> ControlFlow<()> {
        ControlFlow::Break(())
    }

    fn add_constant(&mut self, _value: &'a InlineValue<'a>) -> ControlFlow<()> {
        ControlFlow::Break(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::field;

    use super::*;

    #[test]
    fn test_basic_json_types() {
        validate(json::Null::TYPE, &json!(null)).unwrap();
        validate(json::Bool::TYPE, &json!(true)).unwrap();
        validate(json::Integer::TYPE, &json!(123)).unwrap();
        validate(json::Double::TYPE, &json!(123)).unwrap();
        validate(json::Double::TYPE, &json!(123.4)).unwrap();
        validate(json::String::TYPE, &json!("hello!")).unwrap();
        validate(json::Array::TYPE, &json!([1, 2, 3])).unwrap();
        validate(json::Array::TYPE, &json!([])).unwrap();
        validate(json::Object::TYPE, &json!({"hello": "world"})).unwrap();
        validate(json::Object::TYPE, &json!({})).unwrap();

        validate(json::Any::TYPE, &json!(null)).unwrap();
        validate(json::Any::TYPE, &json!(true)).unwrap();
        validate(json::Any::TYPE, &json!(123)).unwrap();
        validate(json::Any::TYPE, &json!(123)).unwrap();
        validate(json::Any::TYPE, &json!(123.4)).unwrap();
        validate(json::Any::TYPE, &json!("hello!")).unwrap();
        validate(json::Any::TYPE, &json!([1, 2, 3])).unwrap();
        validate(json::Any::TYPE, &json!([])).unwrap();
        validate(json::Any::TYPE, &json!({"hello": "world"})).unwrap();
        validate(json::Any::TYPE, &json!({})).unwrap();
    }

    #[test]
    fn test_std_types() {
        let ints =
            [u8::TYPE, i8::TYPE, u16::TYPE, i16::TYPE, u32::TYPE, i32::TYPE, u64::TYPE, i64::TYPE];
        for int in ints {
            validate(int, &json!(0)).unwrap();
        }

        validate(f32::TYPE, &json!(0)).unwrap();
        validate(f32::TYPE, &json!(0.5)).unwrap();
        validate(f64::TYPE, &json!(0)).unwrap();
        validate(f64::TYPE, &json!(0.5)).unwrap();

        validate(bool::TYPE, &json!(false)).unwrap();

        validate(str::TYPE, &json!("hello")).unwrap();
        validate(String::TYPE, &json!("hello")).unwrap();

        validate(Option::<bool>::TYPE, &json!(null)).unwrap();
        validate(Option::<bool>::TYPE, &json!(true)).unwrap();

        validate(Box::<i32>::TYPE, &json!(123)).unwrap();

        validate(Vec::<i32>::TYPE, &json!([1, 2, 3, 4])).unwrap();
        validate(<[i32]>::TYPE, &json!([1, 2, 3, 4])).unwrap();
        validate(<[i32; 4]>::TYPE, &json!([1, 2, 3, 4])).unwrap();
        validate(<[i32; 3]>::TYPE, &json!([1, 2, 3, 4])).unwrap_err();
        validate(<[i32; 5]>::TYPE, &json!([1, 2, 3, 4])).unwrap_err();

        validate(<()>::TYPE, &json!(null)).unwrap();

        validate(<(u32,)>::TYPE, &json!([123])).unwrap();
        validate(<(u32, bool)>::TYPE, &json!([123, false])).unwrap();
        validate(<(u32, bool, String)>::TYPE, &json!([123, true, "Hello!"])).unwrap();
        validate(<(u32, bool, String, ())>::TYPE, &json!([123, true, "Hello!", null])).unwrap();
    }

    const STRUCT: &Type<'_> = &Type::Struct {
        fields: &[field!(b: bool), field!(n: u32), field!(s: String)],
        extras: None,
    };

    #[test]
    fn test_struct_ok() {
        validate(
            STRUCT,
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
            validate(STRUCT, &json!([1234])),
            Err(vec![ValidationError {
                messages: vec![ValidationErrorMessage::TypeMismatch {
                    expected: Cow::Borrowed(&[ValueType::Object]),
                    actual: ValueType::Array,
                }],
                fields: HashMap::new(),
            }])
        );

        assert_eq!(
            validate(STRUCT, &json!(true)),
            Err(vec![ValidationError {
                messages: vec![ValidationErrorMessage::TypeMismatch {
                    expected: Cow::Borrowed(&[ValueType::Object]),
                    actual: ValueType::Bool,
                }],
                fields: HashMap::new(),
            }])
        );

        assert_eq!(
            validate(STRUCT, &json!("string")),
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
            STRUCT,
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
