// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use nom::branch::alt;
use nom::bytes::complete::{is_not, tag, take_until, take_while};
use nom::character::complete::{alphanumeric1, digit1, line_ending, multispace1, not_line_ending};
use nom::combinator::{map, map_res, value};
use nom::error::{ErrorKind, ParseError};
use nom::multi::many0;
use nom::sequence::{delimited, preceded, separated_pair};
use nom::{self, AsChar, IResult, Parser};
use nom_locate::LocatedSpan;
use std::fmt;
use thiserror::Error;

#[derive(Debug, PartialEq)]
pub(super) struct MouseModel {
    pub(super) identifier: String,
    pub(super) vendor_id: String,
    pub(super) product_id: String,
    pub(super) cpi: u32,
}

#[derive(Debug, PartialEq)]
struct InnerMouseModel {
    identifier: Option<String>,
    vendor_id: Option<String>,
    product_id: Option<String>,
    cpi: Option<u32>,
}

type VendorId = String;
type ProductId = String;

#[derive(Debug, PartialEq, Clone)]
enum Attribute {
    Unknown,
    EndSection,
    Identifier(String),
    USBID(VendorId, ProductId),
    CPI(u32),
}

const KEYWORD_SECTION: &'static str = "Section";
const KEYWORD_END_SECTION: &'static str = "EndSection";
const SECTION_NAME_INPUT_CLASS: &'static str = "InputClass";
const ATTRIBUTE_KEY_IDENTIFIER: &'static str = "Identifier";
const ATTRIBUTE_KEY_MATCHUSBID: &'static str = "MatchUSBID";
const ATTRIBUTE_KEY_OPTION: &'static str = "Option";
const OPTION_MOUSE_CPI: &'static str = r#""Mouse CPI""#;

pub(crate) type NomSpan<'a> = LocatedSpan<&'a str>;

#[derive(Debug, Error, Clone, PartialEq)]
pub(crate) enum XOrgConfParserError {
    NotEof,
    NumericLiteral(String),
    StringLiteral(String),
    InvalidProductId(String),
    InvalidVendorId(String),
    InvalidAttribute(String),
    NotSection(String),
    FailToReachEndSection(String),
    NotEndSection(String),
    NomErr(String, ErrorKind),
}

impl fmt::Display for XOrgConfParserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl ParseError<NomSpan<'_>> for XOrgConfParserError {
    fn from_error_kind(input: NomSpan<'_>, kind: ErrorKind) -> Self {
        XOrgConfParserError::NomErr(input.fragment().to_string(), kind)
    }

    fn append(_input: NomSpan<'_>, _kind: ErrorKind, e: Self) -> Self {
        match e {
            // For a nom error, appended errors are from combinator, it is ok to
            // ignore to make it clear on root cause.
            XOrgConfParserError::NomErr(_, _) => e,

            // For XOrgConfParserError defined error, parser should not invoke
            // append() in error handling.
            _ => {
                unimplemented!();
            }
        }
    }
}

fn is_product_id_char(chr: char) -> bool {
    chr.is_ascii() && (chr.is_hex_digit() || chr == '*')
}

/// allows * to match any device, glob pattern to match using pattern and exact hex number to match.
fn parse_product_id(input: NomSpan<'_>) -> IResult<NomSpan<'_>, String, XOrgConfParserError> {
    let parser = take_while(is_product_id_char);
    helper::map_err(
        map(parser, |s: NomSpan<'_>| s.fragment().to_string()),
        XOrgConfParserError::InvalidProductId,
    )
    .parse(input)
}

fn parse_vendor_id(input: NomSpan<'_>) -> IResult<NomSpan<'_>, String, XOrgConfParserError> {
    let parser = take_while(helper::is_ascii_hex_digit);
    helper::map_err(
        map(parser, |s: NomSpan<'_>| s.fragment().to_string()),
        XOrgConfParserError::InvalidVendorId,
    )
    .parse(input)
}

fn parse_usb_id(input: NomSpan<'_>) -> IResult<NomSpan<'_>, Attribute, XOrgConfParserError> {
    map(separated_pair(parse_vendor_id, tag(":"), parse_product_id), |(vendor_id, product_id)| {
        Attribute::USBID(vendor_id.to_lowercase(), product_id.to_lowercase())
    })
    .parse(input)
}

fn parse_attribute_usb_id(
    input: NomSpan<'_>,
) -> IResult<NomSpan<'_>, Attribute, XOrgConfParserError> {
    preceded(
        tag(ATTRIBUTE_KEY_MATCHUSBID),
        helper::skip_ws_or_comment(helper::quoted(parse_usb_id)),
    )
    .parse(input)
}

fn parse_attribute_identifier(
    input: NomSpan<'_>,
) -> IResult<NomSpan<'_>, Attribute, XOrgConfParserError> {
    let parser =
        preceded(tag(ATTRIBUTE_KEY_IDENTIFIER), helper::skip_ws_or_comment(helper::quoted_str));
    map(parser, |value| Attribute::Identifier(value)).parse(input)
}

fn parse_attribute_cpi(input: NomSpan<'_>) -> IResult<NomSpan<'_>, Attribute, XOrgConfParserError> {
    let parser = preceded(
        tag(ATTRIBUTE_KEY_OPTION),
        helper::skip_ws_or_comment(preceded(
            tag(OPTION_MOUSE_CPI),
            helper::skip_ws_or_comment(helper::quoted_u32),
        )),
    );
    map(parser, |value| Attribute::CPI(value)).parse(input)
}

/// Consume any valid attribute.
fn parse_attribute_any(input: NomSpan<'_>) -> IResult<NomSpan<'_>, Attribute, XOrgConfParserError> {
    let parser = separated_pair(alphanumeric1, multispace1, not_line_ending);
    helper::map_err(map(parser, |_| Attribute::Unknown), XOrgConfParserError::InvalidAttribute)
        .parse(input)
}

fn parse_end_section(input: NomSpan<'_>) -> IResult<NomSpan<'_>, Attribute, XOrgConfParserError> {
    let end = value(Attribute::EndSection, tag(KEYWORD_END_SECTION));
    helper::map_err(end, XOrgConfParserError::NotEndSection).parse(input)
}

fn parse_section_attribute(
    input: NomSpan<'_>,
) -> IResult<NomSpan<'_>, Attribute, XOrgConfParserError> {
    helper::skip_ws_or_comment(alt((
        parse_end_section,
        parse_attribute_identifier,
        parse_attribute_usb_id,
        parse_attribute_cpi,
        parse_attribute_any,
    )))
    .parse(input)
}

fn parse_section(
    input: NomSpan<'_>,
) -> IResult<NomSpan<'_>, Option<InnerMouseModel>, XOrgConfParserError> {
    let res = helper::map_parser_err(
        preceded(tag(KEYWORD_SECTION), helper::skip_ws_or_comment(helper::quoted_str)),
        XOrgConfParserError::NotSection,
    )
    .parse(input);
    let (mut rest, name) = match res {
        Err(e) => {
            return Err(e);
        }
        Ok((rest, name)) => (rest, name),
    };

    // if not a input class section, consumed to end.
    if name != SECTION_NAME_INPUT_CLASS {
        let until_end_section = take_until(KEYWORD_END_SECTION);
        let rest =
            match helper::map_err(until_end_section, XOrgConfParserError::FailToReachEndSection)
                .parse(rest)
            {
                Ok((rest, _)) => rest,
                Err(e) => return Err(e),
            };
        return map(parse_end_section, |_| None).parse(rest);
    }

    let mut mouse_model =
        InnerMouseModel { identifier: None, vendor_id: None, product_id: None, cpi: None };

    loop {
        let attribute = match parse_section_attribute(rest) {
            Ok((r, attr)) => {
                rest = r;
                attr
            }
            Err(e) => {
                return Err(e);
            }
        };

        match attribute {
            Attribute::Unknown => {}
            Attribute::EndSection => {
                break;
            }
            Attribute::Identifier(identifier) => {
                mouse_model.identifier = Some(identifier);
            }
            Attribute::USBID(vid, pid) => {
                mouse_model.vendor_id = Some(vid);
                mouse_model.product_id = Some(pid);
            }
            Attribute::CPI(cpi) => {
                mouse_model.cpi = Some(cpi);
            }
        }
    }

    Ok((rest, Some(mouse_model)))
}

pub(super) fn parse_xorg_file(
    input: NomSpan<'_>,
) -> IResult<NomSpan<'_>, Vec<MouseModel>, XOrgConfParserError> {
    let res = helper::many_until_eof(helper::skip_ws_or_comment(parse_section)).parse(input);
    let models = match res {
        Ok((rest, models)) => {
            // expect all content consumed.
            if !rest.is_empty() {
                return Err(nom::Err::Error(XOrgConfParserError::NotEof));
            }
            models
        }
        Err(e) => {
            return Err(e);
        }
    };

    let models = models
        .into_iter()
        .filter_map(|m| match m {
            Some(m) => {
                let vendor_id = match m.vendor_id {
                    Some(id) => id,
                    None => {
                        return None;
                    }
                };
                let product_id = match m.product_id {
                    Some(id) => id,
                    None => {
                        return None;
                    }
                };
                let cpi = match m.cpi {
                    Some(num) => num,
                    None => {
                        return None;
                    }
                };
                let identifier = match m.identifier {
                    Some(id) => id,
                    None => {
                        return None;
                    }
                };
                Some(MouseModel { identifier, vendor_id, product_id, cpi })
            }
            None => None,
        })
        .collect();

    Ok((NomSpan::new(""), models))
}

mod helper {
    use super::*;

    /// Wraps a parser and replaces its error.
    pub(super) fn map_err<'a, O, P, G>(
        mut parser: P,
        f: G,
    ) -> impl Parser<NomSpan<'a>, Output = O, Error = XOrgConfParserError>
    where
        P: Parser<NomSpan<'a>, Output = O, Error = (NomSpan<'a>, ErrorKind)>,
        G: Fn(/* bad_input: */ String) -> XOrgConfParserError,
    {
        move |input: NomSpan<'a>| {
            parser.parse(input).map_err(|e| match e {
                nom::Err::Error((bad_input, _)) => nom::Err::Error(f(bad_input.to_string())),
                nom::Err::Failure((bad_input, _)) => nom::Err::Failure(f(bad_input.to_string())),
                nom::Err::Incomplete(_) => {
                    unreachable!("Parser should never generate Incomplete errors")
                }
            })
        }
    }

    pub(super) fn map_parser_err<'a, O, P, G>(
        mut parser: P,
        f: G,
    ) -> impl Parser<NomSpan<'a>, Output = O, Error = XOrgConfParserError>
    where
        P: Parser<NomSpan<'a>, Output = O, Error = XOrgConfParserError>,
        G: Fn(String) -> XOrgConfParserError,
    {
        move |input: NomSpan<'a>| {
            parser.parse(input).map_err(|e| match e {
                nom::Err::Error(e) => nom::Err::Error(f(e.to_string())),
                nom::Err::Failure(e) => nom::Err::Failure(f(e.to_string())),
                nom::Err::Incomplete(_) => {
                    unreachable!("Parser should never generate Incomplete errors")
                }
            })
        }
    }

    /// Wraps a parser |f| and discards zero or more whitespace characters or comments before it.
    /// Doesn't discard whitespace after the parser, since this would make it difficult to ensure that
    /// the AST spans contain no trailing whitespace.
    pub(super) fn skip_ws_or_comment<'a, O, F>(
        f: F,
    ) -> impl Parser<NomSpan<'a>, Output = O, Error = XOrgConfParserError>
    where
        F: Parser<NomSpan<'a>, Output = O, Error = XOrgConfParserError>,
    {
        preceded(comment_or_whitespace, f)
    }

    /// xorg conf only allow single line comment after #
    pub(super) fn comment_or_whitespace(
        input: NomSpan<'_>,
    ) -> IResult<NomSpan<'_>, (), XOrgConfParserError> {
        let multispace = value((), multispace1);
        value((), many0(alt((multispace, singleline_comment)))).parse(input)
    }

    /// Parser that matches a single line comment, e.g. "# comment\n".
    pub(super) fn singleline_comment(
        input: NomSpan<'_>,
    ) -> IResult<NomSpan<'_>, (), XOrgConfParserError> {
        value((), (tag("#"), not_line_ending, line_ending)).parse(input)
    }

    /// Applies the parser `f` until reaching the end of the input. `f` must always make progress (i.e.
    /// consume input) and many_until_eof will panic if it doesn't, to prevent infinite loops. Returns
    /// the results of `f` in a Vec.
    pub(super) fn many_until_eof<'a, O, F>(
        mut f: F,
    ) -> impl Parser<NomSpan<'a>, Output = Vec<O>, Error = XOrgConfParserError>
    where
        F: Parser<NomSpan<'a>, Output = O, Error = XOrgConfParserError>,
    {
        move |mut input: NomSpan<'a>| {
            let mut result = vec![];
            loop {
                // Ignore trailing whitespace or comment at the end of the file.
                let (rest, _) = comment_or_whitespace(input)?;
                if rest.fragment().len() == 0 {
                    return Ok((rest, result));
                }

                let (next_input, res) = f.parse(input)?;
                if input.fragment().len() == next_input.fragment().len() {
                    panic!("many_until_eof called on an optional parser. This will result in an infinite loop");
                }
                input = next_input;
                result.push(res);
            }
        }
    }

    pub(super) fn quoted<'a, O, F>(
        f: F,
    ) -> impl Parser<NomSpan<'a>, Output = O, Error = XOrgConfParserError>
    where
        F: Parser<NomSpan<'a>, Output = O, Error = XOrgConfParserError>,
    {
        delimited(tag("\""), f, tag("\""))
    }

    pub(super) fn quoted_str(
        input: NomSpan<'_>,
    ) -> IResult<NomSpan<'_>, String, XOrgConfParserError> {
        let content_or_empty = alt((is_not("\""), tag("")));
        let quoted_parser = delimited(tag("\""), content_or_empty, tag("\""));
        map_err(
            map(quoted_parser, |s: NomSpan<'_>| s.fragment().to_string()),
            XOrgConfParserError::StringLiteral,
        )
        .parse(input)
    }

    pub(super) fn quoted_u32(input: NomSpan<'_>) -> IResult<NomSpan<'_>, u32, XOrgConfParserError> {
        let base10 = map_res(digit1, |s: NomSpan<'_>| u32::from_str_radix(s.fragment(), 10));
        let quoted_parser = delimited(tag("\""), base10, tag("\""));
        map_err(quoted_parser, XOrgConfParserError::NumericLiteral).parse(input)
    }

    pub(super) fn is_ascii_hex_digit(chr: char) -> bool {
        chr.is_ascii() && chr.is_hex_digit()
    }
}

mod tests {
    use super::*;
    use test_case::test_case;

    pub(crate) fn check_result<O: PartialEq + fmt::Debug>(
        result: IResult<NomSpan<'_>, O, XOrgConfParserError>,
        expected_rest: &str,
        expected_output: O,
    ) {
        match result {
            Ok((input, output)) => {
                assert_eq!(input.fragment(), &expected_rest);
                assert_eq!(output, expected_output);
            }
            Err(e) => {
                panic!("{:#?}", e);
            }
        }
    }

    mod helper {
        use super::super::helper::*;
        use super::*;
        use test_case::test_case;

        #[test_case("#123\r\nnext line", "next line"; "crlf")]
        #[test_case("#123\nnext line", "next line"; "lf")]
        #[test_case("#123\nnext line\n", "next line\n"; "more line")]
        #[test_case("#\nnext line\n", "next line\n"; "# only")]
        fn test_singleline_comment(input: &'static str, rest: &'static str) {
            check_result(singleline_comment(NomSpan::new(input)), rest, ());
        }

        #[test]
        fn test_skip_ws_or_comment() {
            let test = || map(tag("test"), |s: NomSpan<'_>| s.fragment().to_string());
            check_result(
                skip_ws_or_comment(test()).parse(NomSpan::new("test")),
                "",
                "test".to_string(),
            );

            check_result(
                skip_ws_or_comment(test()).parse(NomSpan::new(" \n\t\r\ntest")),
                "",
                "test".to_string(),
            );
            check_result(
                skip_ws_or_comment(test()).parse(NomSpan::new("test \n\t\r\n")),
                " \n\t\r\n",
                "test".to_string(),
            );

            check_result(
                skip_ws_or_comment(test()).parse(NomSpan::new(" # test \n test # rest \n ")),
                " # rest \n ",
                "test".to_string(),
            );
        }

        #[test]
        fn test_comment_or_whitespace() {
            let (result, _) = comment_or_whitespace(NomSpan::new("test")).unwrap();
            assert_eq!(result.location_offset(), 0);
            assert_eq!(result.location_line(), 1);
            assert_eq!(result.fragment(), &"test");

            let (result, _) =
                comment_or_whitespace(NomSpan::new(" \n\t\r\ntest \n\t\r\n")).unwrap();
            assert_eq!(result.location_offset(), 5);
            assert_eq!(result.location_line(), 3);
            assert_eq!(result.fragment(), &"test \n\t\r\n");

            let (result, _) =
                comment_or_whitespace(NomSpan::new(" # test \n test # rest \n ")).unwrap();
            assert_eq!(result.location_offset(), 10);
            assert_eq!(result.location_line(), 2);
            assert_eq!(result.fragment(), &"test # rest \n ");
        }

        #[test_case(r#""123""#, "123", ""; "simple")]
        #[test_case(r#""""#, "", ""; "empty")]
        #[test_case(r#""aaa" "bbb""#, "aaa", r#" "bbb""#; "only match the first quoted")]
        fn test_quoted_str(
            input: &'static str,
            want_matched: &'static str,
            want_rest: &'static str,
        ) {
            check_result(quoted_str(NomSpan::new(input)), want_rest, want_matched.to_string());
        }

        #[test]
        fn test_quoted_u32() {
            check_result(quoted_u32(NomSpan::new(r#""123"rest"#)), "rest", 123);
        }
    }

    #[test_case("123f", "123f", ""; "simple")]
    #[test_case("123*", "123*", ""; "with *")]
    #[test_case("*", "*", ""; "only *")]
    #[test_case("123fpo", "123f", "po"; "test rest")]
    fn test_parse_product_id(
        input: &'static str,
        want_matched: &'static str,
        want_rest: &'static str,
    ) {
        check_result(parse_product_id(NomSpan::new(input)), want_rest, want_matched.to_string());
    }

    #[test_case("123f", "123f", ""; "simple")]
    #[test_case("123fpo", "123f", "po"; "test rest")]
    #[test_case("*123", "", "*123"; "does not take wildcard")]
    #[test_case("123*", "123", "*"; "does not take tailing wildcard")]
    fn test_parse_vendor_id(
        input: &'static str,
        want_matched: &'static str,
        want_rest: &'static str,
    ) {
        check_result(parse_vendor_id(NomSpan::new(input)), want_rest, want_matched.to_string());
    }

    #[test_case(
        "123f:456a",
        Attribute::USBID("123f".to_owned(), "456a".to_owned()),
        ""; "simple")]
    #[test_case(
        "123f:45a*",
        Attribute::USBID("123f".to_owned(), "45a*".to_owned()),
        ""; "product id with *")]
    #[test_case(
        "123f:*",
        Attribute::USBID("123f".to_owned(), "*".to_owned()),
        ""; "product id only *")]
    #[test_case(
        "123f:45a*po",
        Attribute::USBID("123f".to_owned(), "45a*".to_owned()),
        "po"; "test rest")]
    fn test_usb_id(input: &'static str, want_matched: Attribute, want_rest: &'static str) {
        check_result(parse_usb_id(NomSpan::new(input)), want_rest, want_matched);
    }

    #[test]
    fn test_parse_end_section() {
        check_result(
            parse_end_section(NomSpan::new("EndSection\naaa")),
            "\naaa",
            Attribute::EndSection,
        );
    }

    #[test_case(
        r#"  Identifier "aaaa""#,
        Attribute::Identifier("aaaa".to_owned()),
        ""; "white space")]
    #[test_case(
        r#"Identifier "aaaa""#,
        Attribute::Identifier("aaaa".to_owned()),
        ""; "Identifier")]
    #[test_case(
        "Identifier \"aaaa\"\nbbb",
        Attribute::Identifier("aaaa".to_owned()),
        "\nbbb"; "Identifier rest")]
    #[test_case(
        r#"MatchUSBID "1234:5678""#,
        Attribute::USBID("1234".to_owned(), "5678".to_owned()),
        ""; "MatchUSBID")]
    #[test_case(
        r#"MatchUSBID "1ABC:2DEF""#,
        Attribute::USBID("1abc".to_owned(), "2def".to_owned()),
        ""; "MatchUSBID lowercase")]
    #[test_case(
        r#"MatchUSBID "1234:567*""#,
        Attribute::USBID("1234".to_owned(), "567*".to_owned()),
        ""; "MatchUSBID with *")]
    #[test_case(
        r#"Option "Mouse CPI" "123""#,
        Attribute::CPI(123),
        ""; "Option Mouse CPI")]
    #[test_case(
        r#"Option "Unknown" "123"\n"#,
        Attribute::Unknown,
        ""; "Option Unknown with value")]
    #[test_case(
        r#"Option "Unknown"\n"#,
        Attribute::Unknown,
        ""; "Option Unknown without value")]
    #[test_case(
        "Unknown \"aaa\"\nbbb",
        Attribute::Unknown,
        "\nbbb"; "Unknown key")]
    #[test_case(
        "EndSection\naaa",
        Attribute::EndSection,
        "\naaa"; "EndSection")]
    fn test_parse_section_attribute(
        input: &'static str,
        result: Attribute,
        wanted_rest: &'static str,
    ) {
        check_result(parse_section_attribute(NomSpan::new(input)), wanted_rest, result);
    }

    #[test]
    fn test_parse_section_not_input_class() {
        let input = r#"Section "SomethingElse"
  SomeAttribute "aaa"
  # a comment
  EndSection
next line"#;
        check_result(parse_section(NomSpan::new(input)), "\nnext line", None);
    }

    #[test]
    fn test_parse_section() {
        let input = r#"Section "InputClass"
  Identifier "A Mouse"
  # a comment
  MatchUSBID "1234:5678"
  Option "Mouse CPI" "4500"
EndSection
next line"#;
        check_result(
            parse_section(NomSpan::new(input)),
            "\nnext line",
            Some(InnerMouseModel {
                identifier: Some("A Mouse".to_owned()),
                vendor_id: Some("1234".to_owned()),
                product_id: Some("5678".to_owned()),
                cpi: Some(4500),
            }),
        );
    }

    #[test]
    fn test_parse_file() {
        let input = r#"
Section "SomethingElse"
  SomeAttribute "aaa"
  # a comment
EndSection

Section "InputClass"
  Identifier "Mouse A"
  # a comment
  MatchUSBID "1234:5678"
  Option "Mouse CPI" "4500"
EndSection

# miss usb id
Section "InputClass"
  Identifier "Mouse B"
  Option "Mouse CPI" "4500"
EndSection

# miss cpi
Section "InputClass"
  Identifier "Mouse C"
  MatchUSBID "1234:5679"
EndSection

# miss Identifier
Section "InputClass"
  MatchUSBID "1234:5679"
  Option "Mouse CPI" "4500"
EndSection
"#;
        check_result(
            parse_xorg_file(NomSpan::new(input)),
            "",
            vec![MouseModel {
                identifier: "Mouse A".to_owned(),
                vendor_id: "1234".to_owned(),
                product_id: "5678".to_owned(),
                cpi: 4500,
            }],
        );
    }
}
