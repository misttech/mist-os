// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{de, Deserialize, Deserializer};
use std::fmt;
use std::marker::PhantomData;

pub fn greater_than_zero<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = i64::deserialize(deserializer)?;
    if value <= 0 {
        return Err(de::Error::custom(format!("i64 out of range: {value}")));
    }
    Ok(value)
}

pub fn one_or_many_strings<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringVec(PhantomData<Vec<String>>);

    impl<'de> de::Visitor<'de> for StringVec {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("either a single string or an array of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![value.to_string()])
        }

        fn visit_seq<S>(self, mut visitor: S) -> Result<Self::Value, S::Error>
        where
            S: de::SeqAccess<'de>,
        {
            let mut out = vec![];
            while let Some(s) = visitor.next_element::<String>()? {
                out.push(s);
            }
            if out.is_empty() {
                Err(de::Error::invalid_length(0, &"expected at least one string"))
            } else {
                Ok(out)
            }
        }
    }

    deserializer.deserialize_any(StringVec(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Eq, PartialEq)]
    struct Test(#[serde(deserialize_with = "super::one_or_many_strings")] Vec<String>);

    #[derive(Debug, Deserialize, Eq, PartialEq)]
    struct TestInt(#[serde(deserialize_with = "super::greater_than_zero")] i64);

    #[fuchsia::test]
    fn parse_valid_single_string() {
        let json = "\"whatever-1982035*()$*H\"";
        let data: Test = serde_json5::from_str(json).expect("deserialize");
        assert_eq!(data, Test(vec!["whatever-1982035*()$*H".into()]));
    }

    #[fuchsia::test]
    fn parse_valid_multiple_strings() {
        let json = "[ \"core/foo:not:a:selector:root/branch:leaf\", \"core/bar:root/twig:leaf\"]";
        let data: Test = serde_json5::from_str(json).expect("deserialize");
        assert_eq!(
            data,
            Test(vec![
                "core/foo:not:a:selector:root/branch:leaf".into(),
                "core/bar:root/twig:leaf".into()
            ])
        );
    }

    #[fuchsia::test]
    fn refuse_invalid_strings() {
        let not_string = "42";
        let bad_list = "[ 42, \"core/bar:not:a:selector:root/twig:leaf\"]";
        let parsed: Result<Test, serde_json5::Error> = serde_json5::from_str(not_string);
        parsed.expect_err("this should fail");
        let parsed: Result<Test, serde_json5::Error> = serde_json5::from_str(bad_list);
        parsed.expect_err("this should fail");
    }

    #[fuchsia::test]
    fn test_greater_than_zero() {
        let data: TestInt = serde_json5::from_str("1").unwrap();
        assert_eq!(data, TestInt(1));
        serde_json5::from_str::<Test>("0").expect_err("0 is not greater than 0");
        serde_json5::from_str::<Test>("-1").expect_err("-1 is not greater than 0");
    }
}
