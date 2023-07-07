// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    error::*,
    parser::{self, ParsingError, VerboseError},
    validate::*,
};
use anyhow::{self, format_err};
use fidl_fuchsia_diagnostics::{
    self, ComponentSelector, Interest, LogInterestSelector, PropertySelector, Selector,
    SelectorArgument, Severity, StringSelector, StringSelectorUnknown, SubtreeSelector,
    TreeSelector,
};
use std::borrow::{Borrow, Cow};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

// Character used to delimit the different sections of an inspect selector,
// the component selector, the tree selector, and the property selector.
pub static SELECTOR_DELIMITER: char = ':';

// Character used to delimit nodes within a component hierarchy path.
static PATH_NODE_DELIMITER: char = '/';

// Character used to escape interperetation of this parser's "special
// characers"; *, /, :, and \.
static ESCAPE_CHARACTER: char = '\\';

static TAB_CHAR: char = '\t';
static SPACE_CHAR: char = ' ';

// Pattern used to encode wildcard.
static WILDCARD_SYMBOL_CHAR: char = '*';

static RECURSIVE_WILDCARD_SYMBOL_STR: &str = "**";

/// Returns true iff a component selector uses the recursive glob.
/// Assumes the selector has already been validated.
pub fn contains_recursive_glob(component_selector: &ComponentSelector) -> bool {
    // Unwrap as a valid selector must contain these fields.
    let last_segment = component_selector.moniker_segments.as_ref().unwrap().last().unwrap();
    string_selector_contains_recursive_glob(last_segment)
}

fn string_selector_contains_recursive_glob(selector: &StringSelector) -> bool {
    match selector {
        StringSelector::StringPattern(pattern) if pattern == RECURSIVE_WILDCARD_SYMBOL_STR => true,
        StringSelector::StringPattern(_)
        | StringSelector::ExactMatch(_)
        | StringSelectorUnknown!() => false,
    }
}

/// Extracts and validates or parses a selector from a `SelectorArgument`.
pub fn take_from_argument<E>(arg: SelectorArgument) -> Result<Selector, Error>
where
    E: for<'a> ParsingError<'a>,
{
    match arg {
        SelectorArgument::StructuredSelector(s) => {
            s.validate()?;
            Ok(s)
        }
        SelectorArgument::RawSelector(r) => parse_selector::<VerboseError>(&r),
        _ => Err(Error::InvalidSelectorArgument),
    }
}

/// Increments the CharIndices iterator and updates the token builder
/// in order to avoid processing characters being escaped by the selector.
fn handle_escaped_char(
    token_builder: &mut String,
    selection_iter: &mut std::str::CharIndices<'_>,
) -> Result<(), anyhow::Error> {
    token_builder.push(ESCAPE_CHARACTER);
    let escaped_char_option: Option<(usize, char)> = selection_iter.next();
    match escaped_char_option {
        Some((_, escaped_char)) => token_builder.push(escaped_char),
        None => {
            return Err(format_err!(
                "Selecter fails verification due to unmatched escape character",
            ));
        }
    }
    Ok(())
}

/// Converts a string into a vector of string tokens representing the unparsed
/// string delimited by the provided delimiter, excluded escaped delimiters.
pub fn tokenize_string(
    untokenized_selector: &str,
    delimiter: char,
) -> Result<Vec<String>, anyhow::Error> {
    let mut token_aggregator = Vec::new();
    let mut curr_token_builder: String = String::new();
    let mut unparsed_selector_iter = untokenized_selector.char_indices();

    while let Some((_, selector_char)) = unparsed_selector_iter.next() {
        match selector_char {
            escape if escape == ESCAPE_CHARACTER => {
                handle_escaped_char(&mut curr_token_builder, &mut unparsed_selector_iter)?;
            }
            selector_delimiter if selector_delimiter == delimiter => {
                if curr_token_builder.is_empty() {
                    return Err(format_err!(
                        "Cannot have empty strings delimited by {}",
                        delimiter
                    ));
                }
                token_aggregator.push(curr_token_builder);
                curr_token_builder = String::new();
            }
            _ => curr_token_builder.push(selector_char),
        }
    }

    // Push the last section of the selector into the aggregator since we don't delimit the
    // end of the selector.
    if curr_token_builder.is_empty() {
        return Err(format_err!(
            "Cannot have empty strings delimited by {}: {}",
            delimiter,
            untokenized_selector
        ));
    }

    token_aggregator.push(curr_token_builder);
    return Ok(token_aggregator);
}

/// Converts an unparsed component selector string into a ComponentSelector.
pub fn parse_component_selector<'a, E>(
    unparsed_component_selector: &'a str,
) -> Result<ComponentSelector, ParseError>
where
    E: ParsingError<'a>,
{
    let result = parser::consuming_component_selector::<E>(&unparsed_component_selector)?;
    Ok(result.into())
}

/// Parses a log severity selector of the form `component_selector#SEVERITY`. For example:
/// core/foo#DEBUG.
pub fn parse_log_interest_selector(selector: &str) -> Result<LogInterestSelector, anyhow::Error> {
    let default_invalid_selector_err = format_err!(
        "Invalid component interest selector: '{}'. Expecting: '/some/moniker/selector#<log-level>'.",
        selector
    );
    let mut parts = selector.split('#');

    // Split each arg into sub string vectors containing strings
    // for component [0] and interest [1] respectively.
    let Some(component) = parts.next() else {
        return Err(default_invalid_selector_err);
    };
    let Some(interest) = parts.next() else {
        return Err(format_err!(
            concat!(
                "Missing <log-level> in selector. Expecting: '{}#<log-level>', ",
                "such as #DEBUG or #INFO."),
            selector
        ));
    };
    if parts.next().is_some() {
        return Err(default_invalid_selector_err);
    }
    let parsed_selector = match parse_component_selector::<VerboseError>(component) {
        Ok(s) => s,
        Err(e) => {
            return Err(format_err!(
                "Invalid component interest selector: '{}'. Error: {}",
                selector,
                e
            ))
        }
    };
    let Some(min_severity) = parse_severity(interest.to_uppercase().as_ref()) else {
        return Err(format_err!(
            concat!(
                "Invalid <log-level> in selector '{}'. Expecting: a min log level ",
                "such as #DEBUG or #INFO."),
            selector
        ));
    };
    Ok(LogInterestSelector {
        selector: parsed_selector,
        interest: Interest { min_severity: Some(min_severity), ..Default::default() },
    })
}

/// Parses a log severity selector of the form `component_selector#SEVERITY` or just `SEVERITY`.
/// For example: `core/foo#DEBUG` or `INFO`.
pub fn parse_log_interest_selector_or_severity(
    selector: &str,
) -> Result<LogInterestSelector, anyhow::Error> {
    if let Some(min_severity) = parse_severity(selector.to_uppercase().as_ref()) {
        return Ok(LogInterestSelector {
            selector: ComponentSelector {
                moniker_segments: Some(vec![StringSelector::StringPattern("**".into())]),
                ..Default::default()
            },
            interest: Interest { min_severity: Some(min_severity), ..Default::default() },
        });
    }
    parse_log_interest_selector(selector)
}

fn parse_severity(severity: &str) -> Option<Severity> {
    match severity {
        "TRACE" => Some(Severity::Trace),
        "DEBUG" => Some(Severity::Debug),
        "INFO" => Some(Severity::Info),
        "WARN" => Some(Severity::Warn),
        "ERROR" => Some(Severity::Error),
        "FATAL" => Some(Severity::Fatal),
        _ => None,
    }
}

/// Converts an unparsed Inspect selector into a ComponentSelector and TreeSelector.
pub fn parse_selector<'a, E>(unparsed_selector: &'a str) -> Result<Selector, Error>
where
    E: ParsingError<'a>,
{
    let result = parser::selector::<E>(&unparsed_selector)?;
    Ok(result.into())
}

/// Remove any comments process a quoted line.
pub fn parse_selector_file<E>(selector_file: &Path) -> Result<Vec<Selector>, Error>
where
    E: for<'a> ParsingError<'a>,
{
    let selector_file = fs::File::open(selector_file)?;
    let mut result = Vec::new();
    let reader = BufReader::new(selector_file);
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        if let Some(selector) = parser::selector_or_comment::<E>(&line)? {
            result.push(selector.into());
        }
    }
    Ok(result)
}

/// Loads all the selectors in the given directory.
pub fn parse_selectors<E>(directory: &Path) -> Result<Vec<Selector>, Error>
where
    E: for<'a> ParsingError<'a>,
{
    let path: PathBuf = directory.to_path_buf();
    let mut selector_vec: Vec<Selector> = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.path().is_dir() {
            return Err(Error::NonFlatDirectory);
        } else {
            selector_vec.append(&mut parse_selector_file::<E>(&entry.path())?);
        }
    }
    Ok(selector_vec)
}

/// Helper method for converting ExactMatch StringSelectors to regex. We must
/// escape all special characters on the behalf of the selector author when converting
/// exact matches to regex.
fn is_special_character(character: char) -> bool {
    character == ESCAPE_CHARACTER
        || character == PATH_NODE_DELIMITER
        || character == SELECTOR_DELIMITER
        || character == WILDCARD_SYMBOL_CHAR
        || character == SPACE_CHAR
        || character == TAB_CHAR
}

/// Sanitizes raw strings from the system such that they align with the
/// special-character and escaping semantics of the Selector format.
///
/// Sanitization escapes the known special characters in the selector language.
pub fn sanitize_string_for_selectors(node: &str) -> Cow<'_, str> {
    if node.is_empty() {
        return Cow::Borrowed(node);
    }

    let mut token_builder = TokenBuilder::new(node);
    for (index, node_char) in node.char_indices() {
        token_builder.maybe_init(index);
        if is_special_character(node_char) {
            token_builder.into_string();
            token_builder.push(ESCAPE_CHARACTER, index);
        }
        token_builder.push(node_char, index);
    }

    token_builder.take()
}

/// Sanitizes a moniker raw string such that it can be used in a selector.
/// Monikers have a restricted set of characters `a-z`, `0-9`, `_`, `.`, `-`.
/// Each moniker segment is separated by a `\`. Segments for collections also contain `:`.
/// That `:` will be escaped.
pub fn sanitize_moniker_for_selectors(moniker: &str) -> String {
    moniker.replace(":", "\\:")
}

pub fn match_moniker_against_component_selector<I, S>(
    mut moniker_segments: I,
    component_selector: &ComponentSelector,
) -> Result<bool, anyhow::Error>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let selector_segments = match &component_selector.moniker_segments {
        Some(ref path_vec) => path_vec,
        None => return Err(format_err!("Component selectors require moniker segments.")),
    };

    for (i, selector_segment) in selector_segments.iter().enumerate() {
        // If the selector is longer than the moniker, then there's no match.
        let Some(moniker_segment) = moniker_segments.next() else {
            return Ok(false);
        };

        // If we are in the last segment and we find a recursive glob, then it's a match.
        if i == selector_segments.len() - 1
            && string_selector_contains_recursive_glob(selector_segment)
        {
            return Ok(true);
        }

        if !match_string(selector_segment, moniker_segment.as_ref()) {
            return Ok(false);
        }
    }

    // We must have consumed all moniker segments.
    if moniker_segments.next().is_some() {
        return Ok(false);
    }

    Ok(true)
}

/// Evaluates a component moniker against a single selector, returning
/// True if the selector matches the component, else false.
///
/// Requires: hierarchy_path is not empty.
///           selectors contains valid Selectors.
pub fn match_component_moniker_against_selector<T>(
    moniker: &[T],
    selector: &Selector,
) -> Result<bool, anyhow::Error>
where
    T: AsRef<str> + std::string::ToString,
{
    selector.validate()?;

    if moniker.is_empty() {
        return Err(format_err!(
            "Cannot have empty monikers, at least the component name is required."
        ));
    }

    // Unwrap is safe because the validator ensures there is a component selector.
    let component_selector = selector.component_selector.as_ref().unwrap();

    match_moniker_against_component_selector(moniker.iter(), component_selector)
}

/// Evaluates a component moniker against a list of selectors, returning
/// all of the selectors which are matches for that moniker.
///
/// Requires: hierarchy_path is not empty.
///           selectors contains valid Selectors.
pub fn match_component_moniker_against_selectors<'a, T, S>(
    moniker: &[T],
    selectors: &'a [S],
) -> Result<Vec<&'a Selector>, anyhow::Error>
where
    T: AsRef<str> + std::string::ToString,
    S: Borrow<Selector>,
{
    if moniker.is_empty() {
        return Err(format_err!(
            "Cannot have empty monikers, at least the component name is required."
        ));
    }

    let selectors = selectors
        .iter()
        .map(|selector| {
            let component_selector = selector.borrow();
            component_selector.validate()?;
            Ok(component_selector)
        })
        .collect::<Result<Vec<&Selector>, anyhow::Error>>();

    selectors?
        .iter()
        .filter_map(|selector| {
            match_component_moniker_against_selector(moniker, selector)
                .map(|is_match| if is_match { Some(*selector) } else { None })
                .transpose()
        })
        .collect::<Result<Vec<&Selector>, anyhow::Error>>()
}

/// Evaluates a component moniker against a list of component selectors, returning
/// all of the component selectors which are matches for that moniker.
///
/// Requires: moniker is not empty.
///           component_selectors contains valid ComponentSelectors.
pub fn match_moniker_against_component_selectors<'a, S, T>(
    moniker: &[T],
    selectors: &'a [S],
) -> Result<Vec<&'a ComponentSelector>, anyhow::Error>
where
    S: Borrow<ComponentSelector> + 'a,
    T: AsRef<str> + std::string::ToString,
{
    if moniker.is_empty() {
        return Err(format_err!(
            "Cannot have empty monikers, at least the component name is required."
        ));
    }

    let component_selectors = selectors
        .iter()
        .map(|selector| {
            let component_selector = selector.borrow();
            component_selector.validate()?;
            Ok(component_selector)
        })
        .collect::<Result<Vec<&ComponentSelector>, anyhow::Error>>();

    component_selectors?
        .iter()
        .filter_map(|selector| {
            match_moniker_against_component_selector(moniker.iter(), selector)
                .map(|is_match| if is_match { Some(*selector) } else { None })
                .transpose()
        })
        .collect::<Result<Vec<&ComponentSelector>, anyhow::Error>>()
}

/// Format a |Selector| as a string.
///
/// Returns the formatted |Selector|, or an error if the |Selector| is invalid.
///
/// Note that the output will always include both a component and tree selector. If your input is
/// simply "moniker" you will likely see "moniker:root" as many clients implicitly append "root" if
/// it is not present (e.g. iquery).
pub fn selector_to_string(selector: Selector) -> Result<String, anyhow::Error> {
    selector.validate()?;

    let component_selector =
        selector.component_selector.ok_or_else(|| format_err!("component selector missing"))?;
    let (node_path, maybe_property_selector) = match selector
        .tree_selector
        .ok_or_else(|| format_err!("tree selector missing"))?
    {
        TreeSelector::SubtreeSelector(SubtreeSelector { node_path, .. }) => (node_path, None),
        TreeSelector::PropertySelector(PropertySelector {
            node_path, target_properties, ..
        }) => (node_path, Some(target_properties)),
        _ => return Err(format_err!("unknown tree selector type")),
    };

    let mut segments = vec![];

    let escape_special_chars = |val: &str| {
        let mut ret = String::with_capacity(val.len());
        for c in val.chars() {
            if is_special_character(c) {
                ret.push('\\');
            }
            ret.push(c);
        }
        ret
    };

    let process_string_selector_vector = |v: Vec<StringSelector>| -> Result<String, anyhow::Error> {
        Ok(v.into_iter()
            .map(|segment| match segment {
                StringSelector::StringPattern(s) => Ok(s),
                StringSelector::ExactMatch(s) => Ok(escape_special_chars(&s)),
                _ => {
                    return Err(format_err!("Unknown string selector type"));
                }
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()?
            .join("/"))
    };

    // Create the component moniker
    segments.push(process_string_selector_vector(
        component_selector
            .moniker_segments
            .ok_or_else(|| format_err!("component selector missing moniker"))?,
    )?);

    // Create the node selector
    segments.push(process_string_selector_vector(node_path)?);

    if let Some(property_selector) = maybe_property_selector {
        segments.push(process_string_selector_vector(vec![property_selector])?);
    }

    Ok(segments.join(":"))
}

/// Match a selector against a target string.
pub fn match_string(selector: &StringSelector, target: impl AsRef<str>) -> bool {
    match selector {
        StringSelector::ExactMatch(s) => s == target.as_ref(),
        StringSelector::StringPattern(pattern) => match_pattern(&pattern, &target.as_ref()),
        _ => false,
    }
}

fn match_pattern(pattern: &str, target: &str) -> bool {
    // Tokenize the string. From: "a*bc*d" to "a, bc, d".
    let mut pattern_tokens = vec![];
    let mut token = TokenBuilder::new(pattern);
    let mut chars = pattern.char_indices();

    while let Some((index, curr_char)) = chars.next() {
        token.maybe_init(index);

        // If we find a backslash then push the next character directly to our new string.
        match curr_char {
            '\\' => {
                match chars.next() {
                    Some((i, c)) => {
                        token.into_string();
                        token.push(c, i);
                    }
                    // We found a backslash without a character to its right. Return false as this
                    // isn't valid.
                    None => return false,
                }
            }
            '*' => {
                if !token.is_empty() {
                    pattern_tokens.push(token.take());
                }
                token = TokenBuilder::new(pattern);
            }
            c => {
                token.push(c, index);
            }
        }
    }

    // Push the remaining token if there's any.
    if !token.is_empty() {
        pattern_tokens.push(token.take());
    }

    // Exit early. We only have *'s.
    if pattern_tokens.is_empty() && !pattern.is_empty() {
        return true;
    }

    // If the pattern doesn't begin with a * and the target string doesn't start with the first
    // pattern token, we can exit.
    if pattern.chars().nth(0) != Some('*') && !target.starts_with(pattern_tokens[0].as_ref()) {
        return false;
    }

    // If the last character of the pattern is not an unescaped * and the target string doesn't end
    // with the last token in the pattern, then we can exit.
    if pattern.chars().rev().nth(0) != Some('*')
        && pattern.chars().rev().nth(1) != Some('\\')
        && !target.ends_with(pattern_tokens[pattern_tokens.len() - 1].as_ref())
    {
        return false;
    }

    // We must find all pattern tokens in the target string in order. If we don't find one then we
    // fail.
    let mut cur_string = target;
    for pattern in pattern_tokens.iter() {
        match cur_string.find(pattern.as_ref()) {
            Some(i) => {
                cur_string = &cur_string[i + pattern.len()..];
            }
            None => {
                return false;
            }
        }
    }

    true
}

// Utility to allow matching the string cloning only when necessary, this is when we run into a
// escaped character.
#[derive(Debug)]
enum TokenBuilder<'a> {
    Init(&'a str),
    Slice { string: &'a str, start: usize, end: Option<usize> },
    String(String),
}

impl<'a> TokenBuilder<'a> {
    fn new(string: &'a str) -> Self {
        Self::Init(string)
    }

    fn maybe_init(&mut self, start_index: usize) {
        match self {
            Self::Init(s) => *self = Self::Slice { string: s, start: start_index, end: None },
            _ => {}
        }
    }

    fn into_string(&mut self) {
        if let Self::Slice { string, start, end: Some(end) } = self {
            *self = Self::String(string[*start..=*end].to_string())
        }
    }

    fn push(&mut self, c: char, index: usize) {
        match self {
            Self::Slice { end, .. } => {
                *end = Some(index);
            }
            Self::String(s) => s.push(c),
            Self::Init(_) => unreachable!(),
        }
    }

    fn take(self) -> Cow<'a, str> {
        match self {
            Self::Slice { string, start, end: Some(end) } => Cow::Borrowed(&string[start..=end]),
            Self::Slice { string, start, end: None } => Cow::Borrowed(&string[start..start]),
            Self::String(s) => Cow::Owned(s),
            Self::Init(_) => unreachable!(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Slice { start, end: Some(end), .. } => start > end,
            Self::Slice { end: None, .. } => true,
            Self::String(s) => s.is_empty(),
            Self::Init(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::prelude::*;
    use tempfile::TempDir;

    #[fuchsia::test]
    fn successful_selector_parsing() {
        let tempdir = TempDir::new().expect("failed to create tmp dir");
        File::create(tempdir.path().join("a.txt"))
            .expect("create file")
            .write_all(
                b"a:b:c

",
            )
            .expect("writing test file");
        File::create(tempdir.path().join("b.txt"))
            .expect("create file")
            .write_all(b"a*/b:c/d/*:*")
            .expect("writing test file");

        File::create(tempdir.path().join("c.txt"))
            .expect("create file")
            .write_all(
                b"// this is a comment
a:b:c
",
            )
            .expect("writing test file");

        assert!(parse_selectors::<VerboseError>(tempdir.path()).is_ok());
    }

    #[fuchsia::test]
    fn unsuccessful_selector_parsing_bad_selector() {
        let tempdir = TempDir::new().expect("failed to create tmp dir");
        File::create(tempdir.path().join("a.txt"))
            .expect("create file")
            .write_all(b"a:b:c")
            .expect("writing test file");
        File::create(tempdir.path().join("b.txt"))
            .expect("create file")
            .write_all(b"**:**:**")
            .expect("writing test file");

        assert!(parse_selectors::<VerboseError>(tempdir.path()).is_err());
    }

    #[fuchsia::test]
    fn unsuccessful_selector_parsing_nonflat_dir() {
        let tempdir = TempDir::new().expect("failed to create tmp dir");
        File::create(tempdir.path().join("a.txt"))
            .expect("create file")
            .write_all(b"a:b:c")
            .expect("writing test file");
        File::create(tempdir.path().join("b.txt"))
            .expect("create file")
            .write_all(b"**:**:**")
            .expect("writing test file");

        std::fs::create_dir_all(tempdir.path().join("nested")).expect("make nested");
        File::create(tempdir.path().join("nested/c.txt"))
            .expect("create file")
            .write_all(b"**:**:**")
            .expect("writing test file");
        assert!(parse_selectors::<VerboseError>(tempdir.path()).is_err());
    }

    #[fuchsia::test]
    fn component_selector_match_test() {
        // Note: We provide the full selector syntax but this test is only validating it
        // against the provided moniker
        let passing_test_cases = vec![
            (r#"echo.cmx:*:*"#, vec!["echo.cmx"]),
            (r#"*/echo.cmx:*:*"#, vec!["abc", "echo.cmx"]),
            (r#"ab*/echo.cmx:*:*"#, vec!["abc", "echo.cmx"]),
            (r#"ab*/echo.cmx:*:*"#, vec!["abcde", "echo.cmx"]),
            (r#"*/ab*/echo.cmx:*:*"#, vec!["123", "abcde", "echo.cmx"]),
            (r#"echo.cmx*:*:*"#, vec!["echo.cmx"]),
            (r#"a/echo*.cmx:*:*"#, vec!["a", "echo1.cmx"]),
            (r#"a/echo*.cmx:*:*"#, vec!["a", "echo.cmx"]),
            (r#"ab*/echo.cmx:*:*"#, vec!["ab", "echo.cmx"]),
            (r#"a/**:*:*"#, vec!["a", "echo.cmx"]),
            (r#"a/**:*:*"#, vec!["a", "b", "echo.cmx"]),
        ];

        for (selector, moniker) in passing_test_cases {
            let parsed_selector = parse_selector::<VerboseError>(selector).unwrap();
            assert!(
                match_component_moniker_against_selector(&moniker, &parsed_selector).unwrap(),
                "Selector {:?} failed to match {:?}",
                selector,
                moniker
            );
        }

        // Note: We provide the full selector syntax but this test is only validating it
        // against the provided moniker
        let failing_test_cases = vec![
            (r#"*:*:*"#, vec!["a", "echo.cmx"]),
            (r#"*/echo.cmx:*:*"#, vec!["123", "abc", "echo.cmx"]),
            (r#"a/**:*:*"#, vec!["b", "echo.cmx"]),
            (r#"e/**:*:*"#, vec!["echo.cmx"]),
        ];

        for (selector, moniker) in failing_test_cases {
            let parsed_selector = parse_selector::<VerboseError>(selector).unwrap();
            assert!(
                !match_component_moniker_against_selector(&moniker, &parsed_selector).unwrap(),
                "Selector {:?} matched {:?}, but was expected to fail",
                selector,
                moniker
            );
        }
    }

    #[fuchsia::test]
    fn multiple_component_selectors_match_test() {
        let selectors = vec![r#"*/echo.cmx"#, r#"ab*/echo.cmx"#, r#"abc/m*"#];
        let moniker = vec!["abc".to_string(), "echo.cmx".to_string()];

        let component_selectors = selectors
            .into_iter()
            .map(|selector| {
                parse_component_selector::<VerboseError>(&selector.to_string()).unwrap()
            })
            .collect::<Vec<_>>();

        let match_res =
            match_moniker_against_component_selectors(moniker.as_slice(), &component_selectors[..]);
        assert!(match_res.is_ok());
        assert_eq!(match_res.unwrap().len(), 2);
    }

    #[fuchsia::test]
    fn selector_to_string_test() {
        // Check that parsing and formatting these selectors results in output identical to the
        // original selector.
        let cases = vec![
            r#"moniker:root"#,
            r#"my/component:root"#,
            r#"my/component:root:a"#,
            r#"a/b/c*ff:root:a"#,
            r#"a/child*:root:a"#,
            r#"a/child:root/a/b/c"#,
            r#"a/child:root/a/b/c:d"#,
            r#"a/child:root/a/b/c*/d"#,
            r#"a/child:root/a/b/c\*/d"#,
            r#"a/child:root/a/b/c:d*"#,
            r#"a/child:root/a/b/c:*d*"#,
            r#"a/child:root/a/b/c:\*d*"#,
            r#"a/child:root/a/b/c:\*d\:\*\\"#,
        ];

        for input in cases {
            let selector = parse_selector::<VerboseError>(input)
                .unwrap_or_else(|e| panic!("Failed to parse '{}': {}", input, e));
            let output = selector_to_string(selector).unwrap_or_else(|e| {
                panic!("Failed to format parsed selector for '{}': {}", input, e)
            });
            assert_eq!(output, input);
        }
    }

    #[fuchsia::test]
    fn exact_match_selector_to_string() {
        let selector = Selector {
            component_selector: Some(ComponentSelector {
                moniker_segments: Some(vec![StringSelector::ExactMatch("a".to_string())]),
                ..Default::default()
            }),
            tree_selector: Some(TreeSelector::SubtreeSelector(SubtreeSelector {
                node_path: vec![StringSelector::ExactMatch("a*:".to_string())],
            })),
            ..Default::default()
        };

        // Check we generate the expected string with escaping.
        let selector_string = selector_to_string(selector).unwrap();
        assert_eq!(r#"a:a\*\:"#, selector_string);

        // Parse the resultant selector, and check that it matches a moniker it is supposed to.
        let parsed = parse_selector::<VerboseError>(&selector_string).unwrap();
        assert!(match_moniker_against_component_selector(
            ["a"].into_iter(),
            parsed.component_selector.as_ref().unwrap()
        )
        .unwrap());
    }

    #[fuchsia::test]
    fn sanitize_moniker_for_selectors_result_is_usable() {
        let selector = parse_selector::<VerboseError>(&format!(
            "{}:root",
            sanitize_moniker_for_selectors("foo/coll:bar/baz")
        ))
        .unwrap();
        let component_selector = selector.component_selector.as_ref().unwrap();
        let moniker = vec!["foo".to_string(), "coll:bar".to_string(), "baz".to_string()];
        assert!(
            match_moniker_against_component_selector(moniker.iter(), &component_selector).unwrap()
        );
    }

    #[fuchsia::test]
    fn escaped_spaces() {
        let selector_str = "foo:bar\\ baz/a*\\ b:quux";
        let selector = parse_selector::<VerboseError>(selector_str).unwrap();
        assert_eq!(
            selector,
            Selector {
                component_selector: Some(ComponentSelector {
                    moniker_segments: Some(vec![StringSelector::ExactMatch("foo".into()),]),
                    ..Default::default()
                }),
                tree_selector: Some(TreeSelector::PropertySelector(PropertySelector {
                    node_path: vec![
                        StringSelector::ExactMatch("bar baz".into()),
                        StringSelector::StringPattern("a*\\ b".into()),
                    ],
                    target_properties: StringSelector::ExactMatch("quux".into())
                })),
                ..Default::default()
            }
        );
    }

    #[fuchsia::test]
    fn match_string_test() {
        // Exact match.
        assert!(match_string(&StringSelector::ExactMatch("foo".into()), "foo"));

        // Valid pattern matches.
        assert!(match_string(&StringSelector::StringPattern("*foo*".into()), "hellofoobye"));
        assert!(match_string(&StringSelector::StringPattern("bar*foo".into()), "barxfoo"));
        assert!(match_string(&StringSelector::StringPattern("bar*foo".into()), "barfoo"));
        assert!(match_string(&StringSelector::StringPattern("bar*foo".into()), "barxfoo"));
        assert!(match_string(&StringSelector::StringPattern("foo*".into()), "foobar"));
        assert!(match_string(&StringSelector::StringPattern("*".into()), "foo"));
        assert!(match_string(&StringSelector::StringPattern("bar*baz*foo".into()), "barxzybazfoo"));
        assert!(match_string(&StringSelector::StringPattern("foo*bar*baz".into()), "foobazbarbaz"));

        // Escaped char.
        assert!(match_string(&StringSelector::StringPattern("foo\\*".into()), "foo*"));

        // Invalid cases.
        assert!(!match_string(&StringSelector::StringPattern("foo\\".into()), "foo\\"));
        assert!(!match_string(&StringSelector::StringPattern("bar*foo".into()), "barxfoox"));
        assert!(!match_string(&StringSelector::StringPattern("m*".into()), "echo.csx"));
        assert!(!match_string(&StringSelector::StringPattern("mx*".into()), "echo.cmx"));
        assert!(!match_string(&StringSelector::StringPattern("m*x*".into()), "echo.cmx"));
        assert!(!match_string(&StringSelector::StringPattern("*foo*".into()), "xbary"));
        assert!(!match_string(
            &StringSelector::StringPattern("foo*bar*baz*qux".into()),
            "foobarbaazqux"
        ));
    }

    #[fuchsia::test]
    fn test_log_interest_selector() {
        assert_eq!(
            parse_log_interest_selector("core/network#FATAL").unwrap(),
            LogInterestSelector {
                selector: parse_component_selector::<VerboseError>("core/network").unwrap(),
                interest: Interest { min_severity: Some(Severity::Fatal), ..Default::default() }
            }
        );
        assert_eq!(
            parse_log_interest_selector("any/component#INFO").unwrap(),
            LogInterestSelector {
                selector: parse_component_selector::<VerboseError>("any/component").unwrap(),
                interest: Interest { min_severity: Some(Severity::Info), ..Default::default() }
            }
        );
    }
    #[test]
    fn test_log_interest_selector_error() {
        assert!(parse_log_interest_selector("anything////#FATAL").is_err());
        assert!(parse_log_interest_selector("core/network").is_err());
        assert!(parse_log_interest_selector("core/network#FAKE").is_err());
    }

    #[test]
    fn test_parse_log_interest_or_severity() {
        for (severity_str, severity) in [
            ("TRACE", Severity::Trace),
            ("DEBUG", Severity::Debug),
            ("INFO", Severity::Info),
            ("WARN", Severity::Warn),
            ("ERROR", Severity::Error),
            ("FATAL", Severity::Fatal),
        ] {
            assert_eq!(
                parse_log_interest_selector_or_severity(severity_str).unwrap(),
                LogInterestSelector {
                    selector: parse_component_selector::<VerboseError>("**").unwrap(),
                    interest: Interest { min_severity: Some(severity), ..Default::default() }
                }
            );
        }

        assert_eq!(
            parse_log_interest_selector_or_severity("foo/bar#DEBUG").unwrap(),
            LogInterestSelector {
                selector: parse_component_selector::<VerboseError>("foo/bar").unwrap(),
                interest: Interest { min_severity: Some(Severity::Debug), ..Default::default() }
            }
        );

        assert!(parse_log_interest_selector_or_severity("RANDOM").is_err());
        assert!(parse_log_interest_selector_or_severity("core/foo#NO#YES").is_err());
    }
}
