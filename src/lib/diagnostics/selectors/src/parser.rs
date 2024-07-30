// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::ParseError;
use crate::types::*;
use crate::validate::{ValidateComponentSelectorExt, ValidateExt, ValidateTreeSelectorExt};
use nom::branch::alt;
use nom::bytes::complete::{escaped, is_not, tag, take_till, take_while};
use nom::character::complete::{alphanumeric1, multispace0, none_of, one_of};
use nom::combinator::{all_consuming, complete, cond, map, opt, peek, recognize, verify};
use nom::error::{ErrorKind, ParseError as NomParseError};
use nom::multi::separated_nonempty_list;
use nom::sequence::{delimited, pair, preceded, separated_pair, tuple};
use nom::IResult;

static ALL_TREE_NAMES_SELECTED_SYMBOL: &'static str = "...";

/// Recognizes 0 or more spaces or tabs.
fn whitespace0<'a, E>(input: &'a str) -> IResult<&'a str, &'a str, E>
where
    E: NomParseError<&'a str>,
{
    take_while(move |c| c == ' ' || c == '\t')(input)
}

/// Parses an input containing any number and type of whitespace at the front.
fn spaced<'a, E, F, O>(parser: F) -> impl Fn(&'a str) -> IResult<&'a str, O, E>
where
    F: Fn(&'a str) -> IResult<&'a str, O, E>,
    E: NomParseError<&'a str>,
{
    preceded(whitespace0, parser)
}

fn extract_conjoined_names<'a, E>(input: &'a str) -> IResult<&'a str, Option<&'a str>, E>
where
    E: NomParseError<&'a str>,
{
    delimited(tag::<&str, &str, E>("["), take_till(|c| c == ']'), tag("]"))(input)
        .map(|(rest, names)| (rest, Some(names)))
}

fn extract_from_quotes<'a, E>(input: &'a str) -> &'a str
where
    E: NomParseError<&'a str>,
{
    if let Ok((_, stripped)) =
        delimited(tag::<&str, &str, E>(r#"""#), take_till(|c| c == '"'), tag(r#"""#))(input)
    {
        stripped
    } else {
        input
    }
}

/// Parses a tree selector, which is a node selector and an optional property selector.
pub fn tree_selector<'a, E>(input: &'a str) -> IResult<&'a str, TreeSelector<'a>, E>
where
    E: NomParseError<&'a str>,
{
    let esc = escaped(none_of(":/\\ \t\n"), '\\', one_of("* \t/:\\"));

    let (rest, unparsed_name_list) = extract_conjoined_names::<E>(input).unwrap_or((input, None));

    let tree_names = if unparsed_name_list == Some(ALL_TREE_NAMES_SELECTED_SYMBOL) {
        Some(TreeNames::All)
    } else {
        unparsed_name_list
            .map(|names: &str| {
                let tree_names = names.split(",");
                tree_names
                    .into_iter()
                    .map(|name| {
                        Ok(extract_from_quotes::<E>(
                            spaced(separated_pair(tag("name"), tag("="), &esc))(name)?.1 .1,
                        ))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .map(|value| value.into())
    };

    let (rest, node_segments) = separated_nonempty_list(tag("/"), &esc)(rest)?;
    let (rest, property_segment) = if peek::<&str, _, E, _>(tag(":"))(rest).is_ok() {
        let (rest, _) = tag(":")(rest)?;
        let (rest, property) = verify(esc, |value: &str| !value.is_empty())(rest)?;
        (rest, Some(property))
    } else {
        (rest, None)
    };
    Ok((
        rest,
        TreeSelector {
            node: node_segments.into_iter().map(|value| value.into()).collect(),
            property: property_segment.map(|value| value.into()),
            tree_names,
        },
    ))
}

/// Parses a component selector.
fn component_selector<'a, E>(input: &'a str) -> IResult<&'a str, ComponentSelector<'a>, E>
where
    E: NomParseError<&'a str>,
{
    let accepted_characters = escaped(
        alt((alphanumeric1, tag("*"), tag("."), tag("-"), tag("_"), tag(">"), tag("<"))),
        '\\',
        tag(":"),
    );
    // Monikers (the first part of a selector) can optionally be preceded by "/" or "./".
    let (rest, segments) = preceded(
        opt(alt((tag("./"), tag("/")))),
        separated_nonempty_list(tag("/"), recognize(accepted_characters)),
    )(input)?;
    Ok((rest, ComponentSelector { segments: segments.into_iter().map(|s| s.into()).collect() }))
}

/// A comment allowed in selector files.
fn comment<'a, E>(input: &'a str) -> IResult<&'a str, &'a str, E>
where
    E: NomParseError<&'a str>,
{
    let (rest, comment) = spaced(preceded(tag("//"), is_not("\n\r")))(input)?;
    if rest.len() > 0 {
        let (rest, _) = one_of("\n\r")(rest)?; // consume the newline character
        return Ok((rest, comment));
    }
    Ok((rest, comment))
}

/// Parses a core selector (component + tree + property). It accepts both raw selectors or
/// selectors wrapped in double quotes. Selectors wrapped in quotes accept spaces in the tree and
/// property names and require internal quotes to be escaped.
fn core_selector<'a, E>(
    input: &'a str,
) -> IResult<&'a str, (ComponentSelector<'a>, TreeSelector<'a>), E>
where
    E: NomParseError<&'a str>,
{
    let (rest, (component, _, tree)) = tuple((component_selector, tag(":"), tree_selector))(input)?;
    Ok((rest, (component, tree)))
}

/// Recognizes selectors, with comments allowed or disallowed.
fn do_parse_selector<'a, E>(
    allow_inline_comment: bool,
) -> impl Fn(&'a str) -> IResult<&'a str, Selector<'a>, E>
where
    E: NomParseError<&'a str>,
{
    map(
        tuple((spaced(core_selector), cond(allow_inline_comment, opt(comment)), multispace0)),
        move |((component, tree), _, _)| Selector { component, tree },
    )
}

/// A fast efficient error that won't provide much information besides the name kind of nom parsers
/// that failed and the position at which it failed.
pub struct FastError;

/// A slower but more user friendly error that will provide information about the chain of parsers
/// that found the error and some context.
pub struct VerboseError;

mod private {
    pub trait Sealed {}

    impl<'a> Sealed for super::FastError {}
    impl<'a> Sealed for super::VerboseError {}
}

/// Implemented by types which can be used to specify the error strategy the parsers should use.
pub trait ParsingError<'a>: private::Sealed {
    type Internal: NomParseError<&'a str>;

    fn to_error(input: &str, err: Self::Internal) -> ParseError;
}

impl<'a> ParsingError<'a> for FastError {
    type Internal = (&'a str, ErrorKind);

    fn to_error(_: &str, (part, error_kind): Self::Internal) -> ParseError {
        ParseError::Fast(part.to_owned(), error_kind)
    }
}

impl<'a> ParsingError<'a> for VerboseError {
    type Internal = nom::error::VerboseError<&'a str>;

    fn to_error(input: &str, err: Self::Internal) -> ParseError {
        ParseError::Verbose(nom::error::convert_error(input, err))
    }
}

/// Parses the input into a `Selector`.
pub fn selector<'a, E>(input: &'a str) -> Result<Selector<'a>, ParseError>
where
    E: ParsingError<'a>,
{
    let result = complete(all_consuming(do_parse_selector::<<E as ParsingError<'_>>::Internal>(
        /*allow_inline_comment=*/ false,
    )))(input);
    match result {
        Ok((_, selector)) => {
            selector.validate()?;
            Ok(selector)
        }
        Err(nom::Err::Error(e) | nom::Err::Failure(e)) => Err(E::to_error(input, e)),
        _ => unreachable!("through the complete combinator we get rid of Incomplete"),
    }
}

/// Parses the input into a `ComponentSelector` ignoring any whitespace around the component
/// selector.
pub fn consuming_tree_selector<'a, E>(input: &'a str) -> Result<TreeSelector<'a>, ParseError>
where
    E: ParsingError<'a>,
{
    let result = nom::combinator::all_consuming::<_, _, <E as ParsingError<'_>>::Internal, _>(
        pair(spaced(tree_selector), multispace0),
    )(input);
    match result {
        Ok((_, (tree_selector, _))) => {
            tree_selector.validate()?;
            Ok(tree_selector)
        }
        Err(nom::Err::Error(e) | nom::Err::Failure(e)) => Err(E::to_error(input, e)),
        _ => unreachable!("through the complete combinator we get rid of Incomplete"),
    }
}

/// Parses the input into a `ComponentSelector` ignoring any whitespace around the component
/// selector.
pub fn consuming_component_selector<'a, E>(
    input: &'a str,
) -> Result<ComponentSelector<'a>, ParseError>
where
    E: ParsingError<'a>,
{
    let result = nom::combinator::all_consuming::<_, _, <E as ParsingError<'_>>::Internal, _>(
        pair(spaced(component_selector), multispace0),
    )(input);
    match result {
        Ok((_, (component_selector, _))) => {
            component_selector.validate()?;
            Ok(component_selector)
        }
        Err(nom::Err::Error(e) | nom::Err::Failure(e)) => Err(E::to_error(input, e)),
        _ => unreachable!("through the complete combinator we get rid of Incomplete"),
    }
}

/// Parses the given input line into a Selector or None.
pub fn selector_or_comment<'a, E>(input: &'a str) -> Result<Option<Selector<'a>>, ParseError>
where
    E: ParsingError<'a>,
{
    let result = complete(all_consuming(alt((
        map(comment, |_| None),
        map(
            do_parse_selector::<<E as ParsingError<'_>>::Internal>(
                /*allow_inline_comment=*/ true,
            ),
            |s| Some(s),
        ),
    ))))(input);
    match result {
        Ok((_, maybe_selector)) => match maybe_selector {
            None => Ok(None),
            Some(selector) => {
                selector.validate()?;
                Ok(Some(selector))
            }
        },
        Err(nom::Err::Error(e) | nom::Err::Failure(e)) => Err(E::to_error(input, e)),
        _ => unreachable!("through the complete combinator we get rid of Incomplete"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn canonical_component_selector_test() {
        let test_vector = vec![
            (
                "a/b/c",
                vec![
                    Segment::ExactMatch("a".into()),
                    Segment::ExactMatch("b".into()),
                    Segment::ExactMatch("c".into()),
                ],
            ),
            (
                "a/*/c",
                vec![
                    Segment::ExactMatch("a".into()),
                    Segment::Pattern("*"),
                    Segment::ExactMatch("c".into()),
                ],
            ),
            (
                "a/b*/c",
                vec![
                    Segment::ExactMatch("a".into()),
                    Segment::Pattern("b*"),
                    Segment::ExactMatch("c".into()),
                ],
            ),
            (
                "a/b/**",
                vec![
                    Segment::ExactMatch("a".into()),
                    Segment::ExactMatch("b".into()),
                    Segment::Pattern("**"),
                ],
            ),
            (
                "core/session\\:id/foo",
                vec![
                    Segment::ExactMatch("core".into()),
                    Segment::ExactMatch("session:id".into()),
                    Segment::ExactMatch("foo".into()),
                ],
            ),
            ("c", vec![Segment::ExactMatch("c".into())]),
            ("<component_manager>", vec![Segment::ExactMatch("<component_manager>".into())]),
            (
                r#"a/*/b/**"#,
                vec![
                    Segment::ExactMatch("a".into()),
                    Segment::Pattern("*"),
                    Segment::ExactMatch("b".into()),
                    Segment::Pattern("**"),
                ],
            ),
        ];

        for (test_string, expected_segments) in test_vector {
            let (_, selector) =
                component_selector::<nom::error::VerboseError<&str>>(&test_string).unwrap();
            assert_eq!(
                expected_segments, selector.segments,
                "For '{}', got: {:?}",
                test_string, selector,
            );

            // Component selectors can start with `/`
            let test_moniker_string = format!("/{test_string}");
            let (_, selector) =
                component_selector::<nom::error::VerboseError<&str>>(&test_moniker_string).unwrap();
            assert_eq!(
                expected_segments, selector.segments,
                "For '{}', got: {:?}",
                test_moniker_string, selector,
            );

            // Component selectors can start with `./`
            let test_moniker_string = format!("./{test_string}");
            let (_, selector) =
                component_selector::<nom::error::VerboseError<&str>>(&test_moniker_string).unwrap();
            assert_eq!(
                expected_segments, selector.segments,
                "For '{}', got: {:?}",
                test_moniker_string, selector,
            );
        }
    }

    #[fuchsia::test]
    fn missing_path_component_selector_test() {
        let component_selector_string = "c";
        let (_, component_selector) =
            component_selector::<nom::error::VerboseError<&str>>(component_selector_string)
                .unwrap();
        let mut path_vec = component_selector.segments;
        assert_eq!(path_vec.pop(), Some(Segment::ExactMatch("c".into())));
        assert!(path_vec.is_empty());
    }

    #[fuchsia::test]
    fn errorful_component_selector_test() {
        let test_vector: Vec<&str> = vec![
            "",
            "a\\",
            r#"a/b***/c"#,
            r#"a/***/c"#,
            r#"a/**/c"#,
            // NOTE: This used to be accepted but not anymore. Spaces shouldn't be a valid component
            // selector character since it's not a valid moniker character.
            " ",
            // NOTE: The previous parser was accepting quotes in component selectors. However, by
            // definition, a component moniker (both in v1 and v2) doesn't allow a `*` in its name.
            r#"a/b\*/c"#,
            r#"a/\*/c"#,
            // Invalid characters
            "a$c/d",
        ];
        for test_string in test_vector {
            let component_selector_result =
                consuming_component_selector::<VerboseError>(test_string);
            assert!(component_selector_result.is_err(), "expected '{}' to fail", test_string);
        }
    }

    #[fuchsia::test]
    fn canonical_tree_selector_test() {
        let test_vector = vec![
            (
                "[name=a]b/c:d",
                vec![Segment::ExactMatch("b".into()), Segment::ExactMatch("c".into())],
                Some(Segment::ExactMatch("d".into())),
                Some(vec!["a"].into()),
            ),
            (
                "[name=a,name=bb]b/c:d",
                vec![Segment::ExactMatch("b".into()), Segment::ExactMatch("c".into())],
                Some(Segment::ExactMatch("d".into())),
                Some(vec!["a", "bb"].into()),
            ),
            (
                "[...]b/c:d",
                vec![Segment::ExactMatch("b".into()), Segment::ExactMatch("c".into())],
                Some(Segment::ExactMatch("d".into())),
                Some(TreeNames::All),
            ),
            (
                "[name=a, name=bb]b/c:d",
                vec![Segment::ExactMatch("b".into()), Segment::ExactMatch("c".into())],
                Some(Segment::ExactMatch("d".into())),
                Some(vec!["a", "bb"].into()),
            ),
            (
                "[name=a, name=\"bb\"]b/c:d",
                vec![Segment::ExactMatch("b".into()), Segment::ExactMatch("c".into())],
                Some(Segment::ExactMatch("d".into())),
                Some(vec!["a", "bb"].into()),
            ),
            (
                r#"[name=a, name="a\/\*\:a"]b/c:d"#,
                vec![Segment::ExactMatch("b".into()), Segment::ExactMatch("c".into())],
                Some(Segment::ExactMatch("d".into())),
                Some(vec!["a", "a/*:a"].into()),
            ),
            (
                "a/b:c",
                vec![Segment::ExactMatch("a".into()), Segment::ExactMatch("b".into())],
                Some(Segment::ExactMatch("c".into())),
                None,
            ),
            (
                "a/*:c",
                vec![Segment::ExactMatch("a".into()), Segment::Pattern("*")],
                Some(Segment::ExactMatch("c".into())),
                None,
            ),
            (
                "a/b:*",
                vec![Segment::ExactMatch("a".into()), Segment::ExactMatch("b".into())],
                Some(Segment::Pattern("*")),
                None,
            ),
            (
                "a/b",
                vec![Segment::ExactMatch("a".into()), Segment::ExactMatch("b".into())],
                None,
                None,
            ),
            (
                r#"a/b\:\*c"#,
                vec![Segment::ExactMatch("a".into()), Segment::ExactMatch("b:*c".into())],
                None,
                None,
            ),
        ];

        for (string, expected_path, expected_property, expected_tree_name) in test_vector {
            let (_, tree_selector) =
                tree_selector::<nom::error::VerboseError<&str>>(string).unwrap();
            assert_eq!(
                tree_selector,
                TreeSelector {
                    node: expected_path,
                    property: expected_property,
                    tree_names: expected_tree_name,
                }
            );
        }
    }

    #[fuchsia::test]
    fn errorful_tree_selector_test() {
        let test_vector = vec![
            // Not allowed due to empty property selector.
            "a/b:",
            // Not allowed due to glob property selector.
            "a/b:**",
            // String literals can't have globs.
            r#"a/b**:c"#,
            // Property selector string literals cant have globs.
            r#"a/b:c**"#,
            "a/b:**",
            // Node path cant have globs.
            "a/**:c",
            // Node path can't be empty
            ":c",
            // Spaces aren't accepted when parsing with allow_spaces=false.
            "a b:c",
            "a*b:\tc",
        ];
        for string in test_vector {
            // prepend a placeholder component selector so that we exercise the validation code.
            let test_selector = format!("a:{}", string);
            assert!(
                selector::<VerboseError>(&test_selector).is_err(),
                "{} should fail",
                test_selector
            );
        }
    }

    #[fuchsia::test]
    fn tree_selector_with_spaces() {
        let with_spaces = vec![
            (
                r#"a\ b:c"#,
                vec![Segment::ExactMatch("a b".into())],
                Some(Segment::ExactMatch("c".into())),
            ),
            (
                r#"ab/\ d:c\ "#,
                vec![Segment::ExactMatch("ab".into()), Segment::ExactMatch(" d".into())],
                Some(Segment::ExactMatch("c ".into())),
            ),
            ("a\\\t*b:c", vec![Segment::Pattern("a\\\t*b")], Some(Segment::ExactMatch("c".into()))),
            (
                r#"a\ "x":c"#,
                vec![Segment::ExactMatch(r#"a "x""#.into())],
                Some(Segment::ExactMatch("c".into())),
            ),
        ];
        for (string, node, property) in with_spaces {
            assert_eq!(
                all_consuming::<_, _, nom::error::VerboseError<&str>, _>(tree_selector)(string)
                    .unwrap()
                    .1,
                TreeSelector { node, property, tree_names: None }
            );
        }

        // Un-escaped quotes aren't accepted when parsing with spaces.
        assert!(all_consuming::<_, _, nom::error::VerboseError<&str>, _>(tree_selector)(
            r#"a/b:"xc"/d"#
        )
        .is_err());
    }

    #[fuchsia::test]
    fn parse_full_selector() {
        assert_eq!(
            selector::<VerboseError>("core/**:some-node/he*re:prop").unwrap(),
            Selector {
                component: ComponentSelector {
                    segments: vec![Segment::ExactMatch("core".into()), Segment::Pattern("**"),],
                },
                tree: TreeSelector {
                    node: vec![Segment::ExactMatch("some-node".into()), Segment::Pattern("he*re"),],
                    property: Some(Segment::ExactMatch("prop".into())),
                    tree_names: None,
                },
            }
        );

        // Ignores whitespace.
        assert_eq!(
            selector::<VerboseError>("   foo:bar  ").unwrap(),
            Selector {
                component: ComponentSelector { segments: vec![Segment::ExactMatch("foo".into())] },
                tree: TreeSelector {
                    node: vec![Segment::ExactMatch("bar".into())],
                    property: None,
                    tree_names: None
                },
            }
        );

        // parses tree names
        assert_eq!(
            selector::<VerboseError>("core/**:[name=foo, name=\"bar\\*\"]some-node/he*re:prop")
                .unwrap(),
            Selector {
                component: ComponentSelector {
                    segments: vec![Segment::ExactMatch("core".into()), Segment::Pattern("**"),],
                },
                tree: TreeSelector {
                    node: vec![Segment::ExactMatch("some-node".into()), Segment::Pattern("he*re"),],
                    property: Some(Segment::ExactMatch("prop".into())),
                    tree_names: Some(vec!["foo", r"bar*"].into()),
                },
            }
        );

        // At least one filter is required when `where` is provided.
        assert!(selector::<VerboseError>("foo:bar where").is_err());
    }

    #[fuchsia::test]
    fn assert_no_trailing_backward_slash() {
        assert!(selector::<VerboseError>(r#"foo:bar:baz\"#).is_err());
    }

    #[fuchsia::test]
    fn parse_full_selector_with_spaces() {
        assert_eq!(
            selector::<VerboseError>(r#"core/foo:some\ node/*:prop"#).unwrap(),
            Selector {
                component: ComponentSelector {
                    segments: vec![
                        Segment::ExactMatch("core".into()),
                        Segment::ExactMatch("foo".into()),
                    ],
                },
                tree: TreeSelector {
                    node: vec![Segment::ExactMatch("some node".into()), Segment::Pattern("*"),],
                    property: Some(Segment::ExactMatch("prop".into())),
                    tree_names: None,
                },
            }
        );
    }
}
