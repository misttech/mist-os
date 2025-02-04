// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/378521591) Remove once implementations below are used.
#![allow(dead_code)]

use crate::errors::UsageError;
use crate::schema::{PropertyScheme, PropertyType};
use serde_json::Value;

const TYPE_TAG: &str = "type";
const ITEMS_TAG: &str = "items";

/// Trait for various parsers. Parsers in this module are intended to be used for generating
/// serde_json::Values from text on a command line or in the value of an environment variables.
/// Note these parsers are, in some ways, biased to produce the desired result. For example, the
/// 'array of' parser can succeed even if the parsed text contains no commas. Such text is
/// interpreted as an array of one item. In another example, the string parser will accept text
/// such as 'true' or '1.0' as strings. Parsers are selected based on the intended type of the
/// parameter or option in question.
pub trait Parser {
    fn parse(&self, parameter_name: &str, text: &str) -> Result<Value, UsageError>;
}

/// Creates a parser for a boolean value. Only 'true' and 'false' are accepted.
pub fn parser_for_boolean() -> Box<dyn Parser + Send + Sync> {
    Box::new(BooleanParser)
}

/// Creates a parser for a number. Accepted numbers may be integers or floating-point numbers,
/// and negative signs are parsed correctly.
pub fn parser_for_number() -> Box<dyn Parser + Send + Sync> {
    Box::new(NumberParser)
}

/// Creates a parser for a string. Only text containing commas is rejected.
pub fn parser_for_string() -> Box<dyn Parser + Send + Sync> {
    Box::new(StringParser)
}

/// Creates a parser for an array of some other type, specified using another (item) parser. If
/// the input text contains no commas, and the item parser accepts it, and an array with a single
/// item will be produced. The item parser will never be asked to parse text containing commas.
pub fn parser_for_array_of(
    item_parser: Box<dyn Parser + Send + Sync>,
) -> Box<dyn Parser + Send + Sync> {
    Box::new(ArrayParser::new(item_parser))
}

/// Creates a parser for values described by `scheme`.
///
/// Booleans, numbers, strings and arrays thereof are supported. Object types are not
/// supported, nor are variant types (types expressed as an array of possible types).
pub fn parser_for_scheme(
    name: &str,
    scheme: &PropertyScheme,
) -> Result<Box<dyn Parser + Send + Sync>, UsageError> {
    match &scheme.property_type {
        PropertyType::String => Ok(Box::new(StringParser)),
        PropertyType::Number => Ok(Box::new(NumberParser)),
        PropertyType::Boolean => Ok(Box::new(BooleanParser)),
        PropertyType::Array => {
            let item_scheme = scheme.items.as_ref().expect("");
            match item_scheme.property_type {
                PropertyType::Array | PropertyType::Object => {
                    Err(UsageError::ArrayOfComplexTypeNotAllowed(String::from(name)))
                }
                _ => Ok(Box::new(ArrayParser::new(parser_for_scheme(name, item_scheme)?))),
            }
        }
        PropertyType::Object => Err(UsageError::ObjectNotAllowed(String::from(name))),
    }
}

struct BooleanParser;

impl Parser for BooleanParser {
    fn parse(&self, parameter_name: &str, text: &str) -> Result<Value, UsageError> {
        match text {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(UsageError::TypeMismatch {
                expected: String::from("boolean"),
                got: String::from(text),
                parameter: String::from(parameter_name),
            }),
        }
    }
}

struct NumberParser;

impl Parser for NumberParser {
    fn parse(&self, parameter_name: &str, text: &str) -> Result<Value, UsageError> {
        let value: Value = serde_json::from_str(text).map_err(|_| UsageError::TypeMismatch {
            expected: String::from("number"),
            got: String::from(text),
            parameter: String::from(parameter_name),
        })?;

        if value.is_number() {
            Ok(value)
        } else {
            Err(UsageError::TypeMismatch {
                expected: String::from("number"),
                got: String::from(text),
                parameter: String::from(parameter_name),
            })
        }
    }
}

struct StringParser;

impl Parser for StringParser {
    fn parse(&self, parameter_name: &str, text: &str) -> Result<Value, UsageError> {
        if text.contains(',') {
            return Err(UsageError::CommasNotAllowed {
                parameter: String::from(parameter_name),
                got: String::from(text),
            });
        }
        Ok(Value::String(String::from(text)))
    }
}

struct ArrayParser {
    item_parser: Box<dyn Parser + Send + Sync>,
}

impl ArrayParser {
    fn new(item_parser: Box<dyn Parser + Send + Sync>) -> Self {
        ArrayParser { item_parser }
    }
}

impl Parser for ArrayParser {
    fn parse(&self, parameter_name: &str, text: &str) -> Result<Value, UsageError> {
        let mut items = vec![];

        for item_text in text.split(',') {
            items.push(self.item_parser.parse(parameter_name, item_text)?);
        }

        Ok(Value::Array(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::fake_schema;
    use serde_json::Number;

    const PARAMETER_NAME: &str = "test_parameter_name";

    #[test]
    fn test_boolean_parser() {
        let under_test = parser_for_boolean();

        assert_eq!(Ok(Value::Bool(true)), under_test.parse(PARAMETER_NAME, "true"));
        assert_eq!(Ok(Value::Bool(false)), under_test.parse(PARAMETER_NAME, "false"));
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("boolean"),
                got: String::from("maybe"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "maybe")
        );
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("boolean"),
                got: String::from("true,false"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "true,false")
        );
    }

    #[test]
    fn test_number_parser() {
        let under_test = parser_for_number();

        assert_eq!(Ok(Value::Number(Number::from(0))), under_test.parse(PARAMETER_NAME, "0"));
        assert_eq!(Ok(Value::Number(Number::from(1))), under_test.parse(PARAMETER_NAME, "1"));
        assert_eq!(Ok(Value::Number(Number::from(-1))), under_test.parse(PARAMETER_NAME, "-1"));
        assert_eq!(
            Ok(Value::Number(Number::from_f64(1.0).unwrap())),
            under_test.parse(PARAMETER_NAME, "1.0")
        );
        assert_eq!(
            Ok(Value::Number(Number::from_f64(-1.0).unwrap())),
            under_test.parse(PARAMETER_NAME, "-1.0")
        );
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("number"),
                got: String::from("0,1"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "0,1")
        );
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("number"),
                got: String::from("notanumber"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "notanumber")
        );
    }

    #[test]
    fn test_string_parser() {
        let under_test = parser_for_string();

        assert_eq!(
            Ok(Value::String(String::from("hello"))),
            under_test.parse(PARAMETER_NAME, "hello")
        );
        assert_eq!(
            Ok(Value::String(String::from("true"))),
            under_test.parse(PARAMETER_NAME, "true")
        );
        assert_eq!(Ok(Value::String(String::from("1"))), under_test.parse(PARAMETER_NAME, "1"));
        assert_eq!(Ok(Value::String(String::from("1.0"))), under_test.parse(PARAMETER_NAME, "1.0"));
        assert_eq!(
            Err(UsageError::CommasNotAllowed {
                parameter: String::from(PARAMETER_NAME),
                got: String::from("a,b"),
            }),
            under_test.parse(PARAMETER_NAME, "a,b")
        );
    }

    #[test]
    fn test_array_parser() {
        let under_test = parser_for_array_of(parser_for_string());

        assert_eq!(
            Ok(Value::Array(vec![Value::String(String::from("hello"))])),
            under_test.parse(PARAMETER_NAME, "hello")
        );
        assert_eq!(
            Ok(Value::Array(vec![
                Value::String(String::from("hello")),
                Value::String(String::from("world"))
            ])),
            under_test.parse(PARAMETER_NAME, "hello,world")
        );
        assert_eq!(
            Ok(Value::Array(vec![Value::String(String::from("1"))])),
            under_test.parse(PARAMETER_NAME, "1")
        );
        assert_eq!(
            Ok(Value::Array(vec![
                Value::String(String::from("1")),
                Value::String(String::from("true"))
            ])),
            under_test.parse(PARAMETER_NAME, "1,true")
        );

        let under_test = parser_for_array_of(parser_for_number());
        assert_eq!(
            Ok(Value::Array(vec![Value::Number(Number::from(1))])),
            under_test.parse(PARAMETER_NAME, "1")
        );
        assert_eq!(
            Ok(Value::Array(vec![Value::Number(Number::from(1)), Value::Number(Number::from(2))])),
            under_test.parse(PARAMETER_NAME, "1,2")
        );
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("number"),
                got: String::from("true"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "1,true")
        );
    }

    #[test]
    fn test_scheme_parser_boolean() {
        let fake_schema = fake_schema();
        let under_test = parser_for_scheme(
            PARAMETER_NAME,
            fake_schema.properties.get("true").expect("fake_schema() contains property 'true'"),
        )
        .unwrap();

        assert_eq!(Ok(Value::Bool(true)), under_test.parse(PARAMETER_NAME, "true"));
        assert_eq!(Ok(Value::Bool(false)), under_test.parse(PARAMETER_NAME, "false"));
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("boolean"),
                got: String::from("maybe"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "maybe")
        );
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("boolean"),
                got: String::from("true,false"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "true,false")
        );
    }

    #[test]
    fn test_scheme_parser_number() {
        let fake_schema = fake_schema();
        let under_test = parser_for_scheme(
            PARAMETER_NAME,
            fake_schema.properties.get("zero").expect("fake_schema() contains property 'zero'"),
        )
        .unwrap();

        assert_eq!(Ok(Value::Number(Number::from(0))), under_test.parse(PARAMETER_NAME, "0"));
        assert_eq!(Ok(Value::Number(Number::from(1))), under_test.parse(PARAMETER_NAME, "1"));
        assert_eq!(Ok(Value::Number(Number::from(-1))), under_test.parse(PARAMETER_NAME, "-1"));
        assert_eq!(
            Ok(Value::Number(Number::from_f64(1.0).unwrap())),
            under_test.parse(PARAMETER_NAME, "1.0")
        );
        assert_eq!(
            Ok(Value::Number(Number::from_f64(-1.0).unwrap())),
            under_test.parse(PARAMETER_NAME, "-1.0")
        );
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("number"),
                got: String::from("0,1"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "0,1")
        );
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("number"),
                got: String::from("notanumber"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "notanumber")
        );
    }

    #[test]
    fn test_scheme_parser_string() {
        let fake_schema = fake_schema();
        let under_test = parser_for_scheme(
            PARAMETER_NAME,
            fake_schema.properties.get("string").expect("fake_schema() contains property 'string'"),
        )
        .unwrap();

        assert_eq!(
            Ok(Value::String(String::from("hello"))),
            under_test.parse(PARAMETER_NAME, "hello")
        );
        assert_eq!(
            Ok(Value::String(String::from("true"))),
            under_test.parse(PARAMETER_NAME, "true")
        );
        assert_eq!(Ok(Value::String(String::from("1"))), under_test.parse(PARAMETER_NAME, "1"));
        assert_eq!(Ok(Value::String(String::from("1.0"))), under_test.parse(PARAMETER_NAME, "1.0"));
        assert_eq!(
            Err(UsageError::CommasNotAllowed {
                parameter: String::from(PARAMETER_NAME),
                got: String::from("a,b"),
            }),
            under_test.parse(PARAMETER_NAME, "a,b")
        );
    }

    #[test]
    fn test_scheme_parser_array() {
        let fake_schema = fake_schema();
        let under_test = parser_for_scheme(
            PARAMETER_NAME,
            fake_schema
                .properties
                .get("array_of_string")
                .expect("fake_schema() contains property 'array_of_string'"),
        )
        .unwrap();

        assert_eq!(
            Ok(Value::Array(vec![Value::String(String::from("hello"))])),
            under_test.parse(PARAMETER_NAME, "hello")
        );
        assert_eq!(
            Ok(Value::Array(vec![
                Value::String(String::from("hello")),
                Value::String(String::from("world"))
            ])),
            under_test.parse(PARAMETER_NAME, "hello,world")
        );
        assert_eq!(
            Ok(Value::Array(vec![Value::String(String::from("1"))])),
            under_test.parse(PARAMETER_NAME, "1")
        );
        assert_eq!(
            Ok(Value::Array(vec![
                Value::String(String::from("1")),
                Value::String(String::from("true"))
            ])),
            under_test.parse(PARAMETER_NAME, "1,true")
        );

        let under_test = parser_for_scheme(
            PARAMETER_NAME,
            fake_schema
                .properties
                .get("array_of_number")
                .expect("fake_schema() contains property 'array_of_number'"),
        )
        .unwrap();
        assert_eq!(
            Ok(Value::Array(vec![Value::Number(Number::from(1))])),
            under_test.parse(PARAMETER_NAME, "1")
        );
        assert_eq!(
            Ok(Value::Array(vec![Value::Number(Number::from(1)), Value::Number(Number::from(2))])),
            under_test.parse(PARAMETER_NAME, "1,2")
        );
        assert_eq!(
            Err(UsageError::TypeMismatch {
                expected: String::from("number"),
                got: String::from("true"),
                parameter: String::from(PARAMETER_NAME),
            }),
            under_test.parse(PARAMETER_NAME, "1,true")
        );
    }
}
