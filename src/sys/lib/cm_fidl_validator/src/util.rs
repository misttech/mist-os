// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::error::*,
    cm_types::{LongName, Name, ParseError, Path, RelativePath, Url, UrlScheme},
    fidl_fuchsia_component_decl as fdecl,
    std::collections::HashMap,
};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum AllowableIds {
    One,
    Many,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CollectionSource {
    Allow,
    Deny,
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) enum TargetId<'a> {
    Component(&'a str, Option<&'a str>),
    Collection(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfferType {
    Static,
    Dynamic,
}

pub(crate) type IdMap<'a> = HashMap<TargetId<'a>, HashMap<&'a str, AllowableIds>>;

pub(crate) fn check_path(
    prop: Option<&String>,
    decl_type: &str,
    keyword: &str,
    errors: &mut Vec<Error>,
) -> bool {
    let conversion_ctor = |s: &String| Path::new(s).map(|_| ());
    check_identifier(conversion_ctor, prop, decl_type, keyword, errors)
}

pub(crate) fn check_relative_path(
    prop: Option<&String>,
    decl_type: &str,
    keyword: &str,
    errors: &mut Vec<Error>,
) -> bool {
    let conversion_ctor = |s: &String| RelativePath::new(s).map(|_| ());
    check_identifier(conversion_ctor, prop, decl_type, keyword, errors)
}

pub(crate) fn check_name(
    prop: Option<&String>,
    decl_type: &str,
    keyword: &str,
    errors: &mut Vec<Error>,
) -> bool {
    let conversion_ctor = |s: &String| Name::try_new(s).map(|_| ());
    check_identifier(conversion_ctor, prop, decl_type, keyword, errors)
}

pub(crate) fn check_dynamic_name(
    prop: Option<&String>,
    decl_type: &str,
    keyword: &str,
    errors: &mut Vec<Error>,
) -> bool {
    let conversion_ctor = |s: &String| LongName::try_new(s).map(|_| ());
    check_identifier(conversion_ctor, prop, decl_type, keyword, errors)
}

fn check_identifier(
    conversion_ctor: impl Fn(&String) -> Result<(), ParseError>,
    prop: Option<&String>,
    decl_type: &str,
    keyword: &str,
    errors: &mut Vec<Error>,
) -> bool {
    let start_err_len = errors.len();
    if let Some(prop) = prop {
        match conversion_ctor(prop) {
            Ok(()) => {}
            Err(ParseError::Empty) => {
                errors.push(Error::empty_field(decl_type, keyword));
            }
            Err(ParseError::TooLong) => {
                errors.push(Error::field_too_long(decl_type, keyword));
            }
            Err(ParseError::InvalidComponentUrl { details }) => {
                errors.push(Error::invalid_url(
                    decl_type,
                    keyword,
                    format!(r#""{prop}": {details}"#),
                ));
            }
            Err(ParseError::InvalidValue | ParseError::NotAName | ParseError::NotAPath) => {
                errors.push(Error::invalid_field(decl_type, keyword));
            }
        }
    } else {
        errors.push(Error::missing_field(decl_type, keyword));
    }
    start_err_len == errors.len()
}

pub(crate) fn check_use_availability(
    decl_type: &str,
    availability: Option<&fdecl::Availability>,
    errors: &mut Vec<Error>,
) {
    match availability {
        Some(fdecl::Availability::Required)
        | Some(fdecl::Availability::Optional)
        | Some(fdecl::Availability::Transitional) => {}
        Some(fdecl::Availability::SameAsTarget) => {
            errors.push(Error::invalid_field(decl_type, "availability"))
        }
        // TODO(dgonyeo): we need to soft migrate the requirement for this field to be set
        //None => errors.push(Error::missing_field(decl_type, "availability")),
        None => (),
    }
}

pub(crate) fn check_route_availability(
    decl: &str,
    availability: Option<&fdecl::Availability>,
    source: Option<&fdecl::Ref>,
    source_name: Option<&String>,
    errors: &mut Vec<Error>,
) {
    match (source, availability) {
        // The availability must be optional or transitional when the source is void.
        (Some(fdecl::Ref::VoidType(_)), Some(fdecl::Availability::Optional))
        | (Some(fdecl::Ref::VoidType(_)), Some(fdecl::Availability::Transitional)) => (),
        (
            Some(fdecl::Ref::VoidType(_)),
            Some(fdecl::Availability::Required | fdecl::Availability::SameAsTarget),
        ) => errors.push(Error::availability_must_be_optional(decl, "availability", source_name)),
        // All other cases are valid
        _ => (),
    }
}

pub fn check_url(
    prop: Option<&String>,
    decl_type: &str,
    keyword: &str,
    errors: &mut Vec<Error>,
) -> bool {
    let conversion_ctor = |s: &String| Url::new(s).map(|_| ());
    check_identifier(conversion_ctor, prop, decl_type, keyword, errors)
}

pub(crate) fn check_url_scheme(
    prop: Option<&String>,
    decl_type: &str,
    keyword: &str,
    errors: &mut Vec<Error>,
) -> bool {
    let conversion_ctor = |s: &String| UrlScheme::new(s).map(|_| ());
    check_identifier(conversion_ctor, prop, decl_type, keyword, errors)
}

#[cfg(test)]
mod tests {

    use {
        super::*,
        cm_types::{MAX_LONG_NAME_LENGTH, MAX_NAME_LENGTH, MAX_PATH_LENGTH},
        lazy_static::lazy_static,
        proptest::prelude::*,
        regex::Regex,
        url::Url,
    };

    const PATH_REGEX_STR: &str = r"(/[^/]+)+";
    const NAME_REGEX_STR: &str = r"[0-9a-zA-Z_][0-9a-zA-Z_\-\.]*";

    lazy_static! {
        static ref PATH_REGEX: Regex =
            Regex::new(&("^".to_string() + PATH_REGEX_STR + "$")).unwrap();
        static ref NAME_REGEX: Regex =
            Regex::new(&("^".to_string() + NAME_REGEX_STR + "$")).unwrap();
        static ref A_BASE_URL: Url = Url::parse("relative:///").unwrap();
    }

    proptest! {
        #[test]
        fn check_path_matches_regex(s in PATH_REGEX_STR) {
            if s.len() < MAX_PATH_LENGTH {
                let mut errors = vec![];
                prop_assert!(check_path(Some(&s), "", "", &mut errors));
                prop_assert!(errors.is_empty());
            }
        }
        #[test]
        fn check_name_matches_regex(s in NAME_REGEX_STR) {
            if s.len() < MAX_NAME_LENGTH {
                let mut errors = vec![];
                prop_assert!(check_name(Some(&s), "", "", &mut errors));
                prop_assert!(errors.is_empty());
            }
        }
        #[test]
        fn check_path_fails_invalid_input(s in ".*") {
            if !PATH_REGEX.is_match(&s) {
                let mut errors = vec![];
                prop_assert!(!check_path(Some(&s), "", "", &mut errors));
                prop_assert!(!errors.is_empty());
            }
        }
        #[test]
        fn check_name_fails_invalid_input(s in ".*") {
            if !NAME_REGEX.is_match(&s) {
                let mut errors = vec![];
                prop_assert!(!check_name(Some(&s), "", "", &mut errors));
                prop_assert!(!errors.is_empty());
            }
        }
        // NOTE: The Url crate's parser is used to validate legal URLs. Testing
        // random strings against component URL validation is redundant, so
        // a `check_url_fails_invalid_input` is not necessary (and would be
        // non-trivial to do using just a regular expression).


    }

    fn check_test<F>(check_fn: F, input: &str, expected_res: Result<(), ErrorList>)
    where
        F: FnOnce(Option<&String>, &str, &str, &mut Vec<Error>) -> bool,
    {
        let mut errors = vec![];
        let res: Result<(), ErrorList> =
            match check_fn(Some(&input.to_string()), "FooDecl", "foo", &mut errors) {
                true => Ok(()),
                false => Err(ErrorList::new(errors)),
            };
        assert_eq!(
            format!("{:?}", res),
            format!("{:?}", expected_res),
            "Unexpected result for input: '{}'\n{}",
            input,
            {
                match Url::parse(input).or_else(|err| {
                    if err == url::ParseError::RelativeUrlWithoutBase {
                        A_BASE_URL.join(input)
                    } else {
                        Err(err)
                    }
                }) {
                    Ok(url) => format!(
                        "scheme={}, host={:?}, path={}, fragment={:?}",
                        url.scheme(),
                        url.host_str(),
                        url.path(),
                        url.fragment()
                    ),
                    Err(_) => "".to_string(),
                }
            }
        );
    }

    macro_rules! test_string_checks {
        (
            $(
                $test_name:ident => {
                    check_fn = $check_fn:expr,
                    input = $input:expr,
                    result = $result:expr,
                },
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    check_test($check_fn, $input, $result);
                }
            )+
        }
    }

    test_string_checks! {
        // path
        test_identifier_path_valid => {
            check_fn = check_path,
            input = "/foo/bar",
            result = Ok(()),
        },
        test_identifier_path_invalid_empty => {
            check_fn = check_path,
            input = "",
            result = Err(ErrorList::new(vec![Error::empty_field("FooDecl", "foo")])),
        },
        test_identifier_path_invalid_root => {
            check_fn = check_path,
            input = "/",
            result = Err(ErrorList::new(vec![Error::invalid_field("FooDecl", "foo")])),
        },
        test_identifier_path_invalid_relative => {
            check_fn = check_path,
            input = "foo/bar",
            result = Err(ErrorList::new(vec![Error::invalid_field("FooDecl", "foo")])),
        },
        test_identifier_path_invalid_trailing => {
            check_fn = check_path,
            input = "/foo/bar/",
            result = Err(ErrorList::new(vec![Error::invalid_field("FooDecl", "foo")])),
        },
        test_identifier_path_too_long => {
            check_fn = check_path,
            input = &format!("/{}", "a".repeat(1024)),
            result = Err(ErrorList::new(vec![Error::field_too_long("FooDecl", "foo")])),
        },

        // name
        test_identifier_dynamic_name_valid => {
            check_fn = check_dynamic_name,
            input = &format!("{}", "a".repeat(MAX_LONG_NAME_LENGTH)),
            result = Ok(()),
        },
        test_identifier_name_valid => {
            check_fn = check_name,
            input = "abcdefghijklmnopqrstuvwxyz0123456789_-.",
            result = Ok(()),
        },
        test_identifier_name_invalid => {
            check_fn = check_name,
            input = "^bad",
            result = Err(ErrorList::new(vec![Error::invalid_field("FooDecl", "foo")])),
        },
        test_identifier_name_too_long => {
            check_fn = check_name,
            input = &format!("{}", "a".repeat(MAX_NAME_LENGTH + 1)),
            result = Err(ErrorList::new(vec![Error::field_too_long("FooDecl", "foo")])),
        },
        test_identifier_dynamic_name_too_long => {
            check_fn = check_dynamic_name,
            input = &format!("{}", "a".repeat(MAX_LONG_NAME_LENGTH + 1)),
            result = Err(ErrorList::new(vec![Error::field_too_long("FooDecl", "foo")])),
        },

        // url
        test_identifier_url_valid => {
            check_fn = check_url,
            input = "my+awesome-scheme.2://abc123!@$%.com",
            result = Ok(()),
        },
        test_host_path_url_valid => {
            check_fn = check_url,
            input = "some-scheme://host/path/segments",
            result = Ok(()),
        },
        test_host_path_resource_url_valid => {
            check_fn = check_url,
            input = "some-scheme://host/path/segments#meta/comp.cm",
            result = Ok(()),
        },
        test_nohost_path_resource_url_valid => {
            check_fn = check_url,
            input = "some-scheme:///path/segments#meta/comp.cm",
            result = Ok(()),
        },
        test_relative_path_resource_url_valid => {
            check_fn = check_url,
            input = "path/segments#meta/comp.cm",
            result = Ok(()),
        },
        test_relative_resource_url_valid => {
            check_fn = check_url,
            input = "path/segments#meta/comp.cm",
            result = Ok(()),
        },
        test_identifier_url_host_pound_invalid => {
            check_fn = check_url,
            input = "my+awesome-scheme.2://abc123!@#$%.com",
            result = Err(ErrorList::new(vec![Error::invalid_url("FooDecl", "foo", "\"my+awesome-scheme.2://abc123!@#$%.com\": Malformed URL: EmptyHost.")])),
        },
        test_identifier_url_invalid => {
            check_fn = check_url,
            input = "fuchsia-pkg://",
            result = Err(ErrorList::new(vec![Error::invalid_url("FooDecl", "foo","\"fuchsia-pkg://\": URL is missing either `host`, `path`, and/or `resource`.")])),
        },
        test_url_invalid_port => {
            check_fn = check_url,
            input = "scheme://invalid-port:99999999/path#frag",
            result = Err(ErrorList::new(vec![
                Error::invalid_url("FooDecl", "foo", "\"scheme://invalid-port:99999999/path#frag\": Malformed URL: InvalidPort."),
            ])),
        },
        test_identifier_url_too_long => {
            check_fn = check_url,
            input = &format!("fuchsia-pkg://{}", "a".repeat(4083)),
            result = Err(ErrorList::new(vec![Error::field_too_long("FooDecl", "foo")])),
        },
    }
}
