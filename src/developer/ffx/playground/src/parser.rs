// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the grammar for the FFX playground command language.
//! We've documented the rules with a modified BNF. Some conventions:
//!   * The character `⊔` is the name of the rule that matches whitespace. It
//!     appears as a defined non-terminal where we define what constitutes
//!     whitespace in the grammar.
//!   * `A ← B C` is a normal rule. `A ←⊔ B C` is a rule where there is implicit
//!     whitespace between the elements.
//!   * `/` indicates ordered alternation, meaning an alternation where the left
//!     side of the operator will be parsed first, and only if it fails to match
//!     will the right side be attempted. This is not an unusual convention, but
//!     the operation itself is unusual if one is used to looking at BNF for
//!     context free grammars rather than combinator parsers. The normal
//!     alternation operator, usually denoted `|`, will likely not appear in
//!     this grammar.

use nom::branch::alt;
use nom::bytes::complete::{tag, take, take_while1};
use nom::character::complete::{
    alphanumeric1, anychar, char as chr, digit1, hex_digit1, none_of, one_of,
};
use nom::combinator::{
    all_consuming, cond, flat_map, map, map_parser, not, opt, peek, recognize, rest, verify,
};
use nom::error as nom_error;
use nom::multi::{
    fold_many0, many0, many0_count, many1, many_till, separated_list, separated_nonempty_list,
};
use nom::sequence::{delimited, pair, preceded, separated_pair, terminated, tuple};
use nom_locate::LocatedSpan;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// Value indicating whether a variable is mutable or constant
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Mutability {
    Constant,
    Mutable,
}

impl Mutability {
    pub fn is_constant(&self) -> bool {
        matches!(self, Mutability::Constant)
    }
}

#[derive(Debug)]
enum Error<'a> {
    #[allow(dead_code)]
    Nom(Span<'a>, nom_error::ErrorKind),
    Signal,
}

impl<'a> nom_error::ParseError<ESpan<'a>> for Error<'a> {
    fn from_error_kind(input: ESpan<'a>, kind: nom_error::ErrorKind) -> Self {
        Error::Nom(input.strip_parse_state(), kind)
    }

    fn append(_input: ESpan<'a>, _kind: nom_error::ErrorKind, other: Self) -> Self {
        other
    }
}

type IResult<'a, O> = nom::IResult<ESpan<'a>, O, Error<'a>>;

/// Global parser state which keeps track of backtracking as well as tab completion.
#[derive(Debug, Clone)]
struct ParseState<'a> {
    errors_accepted: Vec<(Span<'a>, String)>,
    trying: Option<(Span<'a>, String)>,
    errors_new: Vec<(Span<'a>, String)>,
}

impl<'a> ParseState<'a> {
    fn new() -> Self {
        ParseState { errors_accepted: Vec::new(), trying: None, errors_new: Vec::new() }
    }
}

pub type Span<'a> = LocatedSpan<&'a str>;
type ESpan<'a> = LocatedSpan<&'a str, Rc<RefCell<ParseState<'a>>>>;

trait StripParseState<'a> {
    fn strip_parse_state(self) -> Span<'a>;
}

impl<'a> StripParseState<'a> for ESpan<'a> {
    fn strip_parse_state(self) -> Span<'a> {
        // Safe because we're basically just stripping away unrelated fields from the
        // safety-sensitive state and leaving it unchanged.
        // TODO: A later version of nom_locate has map_extra which will let us
        // go without this unsafe block.
        unsafe {
            Span::new_from_raw_offset(
                self.location_offset(),
                self.location_line(),
                *self.fragment(),
                (),
            )
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node<'a> {
    Add(Box<Node<'a>>, Box<Node<'a>>),
    Async(Box<Node<'a>>),
    Assignment(Box<Node<'a>>, Box<Node<'a>>),
    BareString(Span<'a>),
    Block(Vec<Node<'a>>),
    Divide(Box<Node<'a>>, Box<Node<'a>>),
    EQ(Box<Node<'a>>, Box<Node<'a>>),
    FunctionDecl { identifier: Span<'a>, parameters: ParameterList<'a>, body: Box<Node<'a>> },
    GE(Box<Node<'a>>, Box<Node<'a>>),
    GT(Box<Node<'a>>, Box<Node<'a>>),
    Identifier(Span<'a>),
    If { condition: Box<Node<'a>>, body: Box<Node<'a>>, else_: Option<Box<Node<'a>>> },
    Integer(Span<'a>),
    Invocation(Span<'a>, Vec<Node<'a>>),
    Iterate(Box<Node<'a>>, Box<Node<'a>>),
    LE(Box<Node<'a>>, Box<Node<'a>>),
    LT(Box<Node<'a>>, Box<Node<'a>>),
    Label(Span<'a>),
    Lambda { parameters: ParameterList<'a>, body: Box<Node<'a>> },
    List(Vec<Node<'a>>),
    LogicalAnd(Box<Node<'a>>, Box<Node<'a>>),
    LogicalNot(Box<Node<'a>>),
    LogicalOr(Box<Node<'a>>, Box<Node<'a>>),
    Lookup(Box<Node<'a>>, Box<Node<'a>>),
    Multiply(Box<Node<'a>>, Box<Node<'a>>),
    NE(Box<Node<'a>>, Box<Node<'a>>),
    Negate(Box<Node<'a>>),
    Object(Option<Span<'a>>, Vec<(Node<'a>, Node<'a>)>),
    Pipe(Box<Node<'a>>, Box<Node<'a>>),
    Program(Vec<Node<'a>>),
    Range(Box<Option<Node<'a>>>, Box<Option<Node<'a>>>, bool),
    Real(Span<'a>),
    String(Span<'a>),
    Subtract(Box<Node<'a>>, Box<Node<'a>>),
    VariableDecl { identifier: Span<'a>, value: Box<Node<'a>>, mutability: Mutability },
    True,
    False,
    Null,
    Error,
}

#[cfg(test)]
impl<'a> Node<'a> {
    /// Checks whether two nodes refer to equivalent content. This is a rough
    /// sort of comparison suitable for comparing test case output to a
    /// reference node tree. To use a normal derived comparison, we'd have to
    /// have all our spans in our reference outputs have correct
    /// line/column/offset information. The tests are hideous to specify that way.
    fn content_eq<'b>(&self, other: &Node<'b>) -> bool {
        use Node::*;
        match (self, other) {
            (Add(a, b), Add(c, d)) => a.content_eq(c) && b.content_eq(d),
            (Async(a), Async(b)) => a.content_eq(b),
            (Assignment(a, b), Assignment(c, d)) => a.content_eq(c) && b.content_eq(d),
            (BareString(a), BareString(b)) => a.fragment() == b.fragment(),
            (Divide(a, b), Divide(c, d)) => a.content_eq(c) && b.content_eq(d),
            (EQ(a, b), EQ(c, d)) => a.content_eq(c) && b.content_eq(d),
            (GE(a, b), GE(c, d)) => a.content_eq(c) && b.content_eq(d),
            (GT(a, b), GT(c, d)) => a.content_eq(c) && b.content_eq(d),
            (Identifier(a), Identifier(b)) => a.fragment() == b.fragment(),
            (Integer(a), Integer(b)) => a.fragment() == b.fragment(),
            (
                If { condition: condition_a, body: body_a, else_: else_a },
                If { condition: condition_b, body: body_b, else_: else_b },
            ) => {
                condition_a.content_eq(condition_b)
                    && body_a.content_eq(body_b)
                    && match (else_a, else_b) {
                        (Some(a), Some(b)) => a.content_eq(b),
                        (None, None) => true,
                        _ => false,
                    }
            }
            (Invocation(a, a_args), Invocation(b, b_args)) => {
                a.fragment() == b.fragment()
                    && a_args.len() == b_args.len()
                    && a_args.iter().zip(b_args.iter()).all(|(a, b)| a.content_eq(b))
            }
            (Iterate(a, b), Iterate(c, d)) => a.content_eq(c) && b.content_eq(d),
            (LE(a, b), LE(c, d)) => a.content_eq(c) && b.content_eq(d),
            (LT(a, b), LT(c, d)) => a.content_eq(c) && b.content_eq(d),
            (List(a), List(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| a.content_eq(b))
            }
            (LogicalAnd(a, b), LogicalAnd(c, d)) => a.content_eq(c) && b.content_eq(d),
            (LogicalNot(a), LogicalNot(b)) => a.content_eq(b),
            (LogicalOr(a, b), LogicalOr(c, d)) => a.content_eq(c) && b.content_eq(d),
            (Lookup(a, b), Lookup(c, d)) => a.content_eq(c) && b.content_eq(d),
            (Multiply(a, b), Multiply(c, d)) => a.content_eq(c) && b.content_eq(d),
            (NE(a, b), NE(c, d)) => a.content_eq(c) && b.content_eq(d),
            (Negate(a), Negate(b)) => a.content_eq(b),
            (Object(label_a, a), Object(label_b, b)) => {
                label_a
                    .map(|label_a| {
                        label_b
                            .map(|label_b| label_b.fragment() == label_a.fragment())
                            .unwrap_or(false)
                    })
                    .unwrap_or(label_b.is_none())
                    && a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|((a1, a2), (b1, b2))| a1.content_eq(b1) && a2.content_eq(b2))
            }
            (Pipe(a, b), Pipe(c, d)) => a.content_eq(c) && b.content_eq(d),
            (Program(a), Program(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| a.content_eq(b))
            }
            (Block(a), Block(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| a.content_eq(b))
            }
            (Real(a), Real(b)) => a.fragment() == b.fragment(),
            (String(a), String(b)) => a.fragment() == b.fragment(),
            (Subtract(a, b), Subtract(c, d)) => a.content_eq(c) && b.content_eq(d),
            (
                FunctionDecl { identifier: identifier_a, parameters: parameters_a, body: body_a },
                FunctionDecl { identifier: identifier_b, parameters: parameters_b, body: body_b },
            ) => {
                identifier_a.fragment() == identifier_b.fragment()
                    && body_a.content_eq(body_b)
                    && parameters_a.content_eq(parameters_b)
            }
            (
                Lambda { parameters: parameters_a, body: body_a },
                Lambda { parameters: parameters_b, body: body_b },
            ) => body_a.content_eq(body_b) && parameters_a.content_eq(parameters_b),
            (
                VariableDecl { identifier: identifier_a, value: value_a, mutability: mutability_a },
                VariableDecl { identifier: identifier_b, value: value_b, mutability: mutability_b },
            ) => {
                identifier_a.fragment() == identifier_b.fragment()
                    && value_a.content_eq(value_b)
                    && mutability_a == mutability_b
            }
            (Error, Error) => true,
            _ => false,
        }
    }
}

/// Represents the parameters of a function or lambda.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterList<'a> {
    pub parameters: Vec<Span<'a>>,
    pub optional_parameters: Vec<Span<'a>>,
    pub variadic: Option<Span<'a>>,
}

#[cfg(test)]
impl ParameterList<'_> {
    fn content_eq(&self, other: &Self) -> bool {
        self.parameters.len() == other.parameters.len()
            && self
                .parameters
                .iter()
                .zip(other.parameters.iter())
                .all(|(x, y)| x.fragment() == y.fragment())
            && self.optional_parameters.len() == other.optional_parameters.len()
            && self
                .optional_parameters
                .iter()
                .zip(other.optional_parameters.iter())
                .all(|(x, y)| x.fragment() == y.fragment())
            && match (&self.variadic, &other.variadic) {
                (Some(a), Some(b)) => a.fragment() == b.fragment(),
                (None, None) => true,
                _ => false,
            }
    }
}

/// Result of parsing. Contains a Node which roots the parse tree and a list of errors.
pub struct ParseResult<'a> {
    pub tree: Node<'a>,
    pub errors: Vec<(Span<'a>, String)>,
}

impl<'a> From<&'a str> for ParseResult<'a> {
    fn from(text: &'a str) -> ParseResult<'a> {
        let text = ESpan::new_extra(text, Rc::new(RefCell::new(ParseState::new())));
        let (end_pos, tree) = recover(alt((
            terminated(
                map(ws_around(program), Node::Program),
                alt((not(anychar), map(err_skip("Trailing characters {}", rest), |_| ()))),
            ),
            err_skip("Unrecoverable parse error", rest),
        )))(text)
        .expect("Incorrectly handled parse error");

        let mut errors = std::mem::take(&mut end_pos.extra.borrow_mut().errors_accepted);
        if let Some(last) = end_pos.extra.borrow_mut().trying.take() {
            errors.push(last)
        }

        ParseResult { tree, errors }
    }
}

/// Try to recover from errors that occurred beneath this node.
fn recover<'a, F: Fn(ESpan<'a>) -> IResult<'a, X>, X>(
    f: F,
) -> impl Fn(ESpan<'a>) -> IResult<'a, X> {
    move |mut input| {
        input.extra = Rc::new(RefCell::new(ParseState::new()));
        let mut to_try = VecDeque::new();

        loop {
            match (&f)(input.clone()) {
                Err(e) => {
                    let mut recovery = input.extra.borrow_mut();
                    let mut errors_new = std::mem::take(&mut recovery.errors_new);
                    errors_new.sort_by(|x, y| x.0.location_offset().cmp(&y.0.location_offset()));
                    if let Some((first, _)) = errors_new.last() {
                        let offset = first.location_offset();
                        if let Some(trying) = recovery.trying.take() {
                            if offset > trying.0.location_offset() + trying.0.fragment().len() {
                                recovery.errors_accepted.push(trying);
                                recovery.trying = errors_new.pop();
                            } else {
                                recovery.trying = to_try.pop_front();
                                if recovery.trying.is_some() {
                                    continue;
                                } else {
                                    break (Err(e));
                                }
                            }
                        } else {
                            recovery.trying = errors_new.pop();
                        }
                        to_try.clear();
                        to_try.extend(
                            errors_new
                                .into_iter()
                                .rev()
                                .take_while(|x| x.0.location_offset() == offset),
                        );
                    } else {
                        recovery.trying = to_try.pop_front();
                        if !recovery.trying.is_some() {
                            break (Err(e));
                        }
                    }
                }
                other => break other,
            }
        }
    }
}

/// Handle an error by skipping some parsed data. If the skip parser fails the error handling
/// fails.
fn err_skip<'a, F: Fn(ESpan<'a>) -> IResult<'a, X>, S: ToString + 'a, X>(
    msg: S,
    f: F,
) -> impl Fn(ESpan<'a>) -> IResult<'a, Node<'a>> {
    move |input| {
        let (out_span, result) = recognize(&f)(input)?;
        let message = msg.to_string().replace("{}", *result.fragment());
        let err = (result.strip_parse_state(), message);
        let mut recovery = out_span.extra.borrow_mut();

        if recovery.errors_accepted.iter().any(|x| x == &err)
            || recovery.trying.as_ref() == Some(&err)
        {
            std::mem::drop(recovery);
            Ok((out_span, Node::Error))
        } else {
            recovery.errors_new.push(err);
            Err(nom::Err::Error(Error::Signal))
        }
    }
}

/// Handle an error by simply marking it with an error node and moving along.
fn err_insert<'a, S: ToString + 'a>(msg: S) -> impl Fn(ESpan<'a>) -> IResult<'a, Node<'a>> {
    err_skip(msg, |x| Ok((x, Node::Error)))
}

/// Handle an error by reporting it but introduce no node.
fn err_note<'a, S: ToString + 'a>(msg: S) -> impl Fn(ESpan<'a>) -> IResult<'a, ()> {
    map(err_insert(msg), |_| ())
}

/// Same as `tag` but inserts an error if the tag is missing.
fn ex_tag<'a>(s: &'a str) -> impl Fn(ESpan<'a>) -> IResult<'a, ()> + 'a {
    let s = s.to_owned();
    move |x| alt((map(tag(s.as_str()), |_| ()), err_note(format!("Expected '{}'", s))))(x)
}

/// Match optional whitespace before the given combinator.
fn ws_before<'a, F: Fn(ESpan<'a>) -> IResult<'a, X>, X>(
    f: F,
) -> impl Fn(ESpan<'a>) -> IResult<'a, X> {
    preceded(opt(whitespace), f)
}

/// Match optional whitespace after the given combinator.
fn ws_after<'a, F: Fn(ESpan<'a>) -> IResult<'a, X>, X>(
    f: F,
) -> impl Fn(ESpan<'a>) -> IResult<'a, X> {
    terminated(f, opt(whitespace))
}

/// Match optional whitespace around the given combinator.
fn ws_around<'a, F: Fn(ESpan<'a>) -> IResult<'a, X>, X>(
    f: F,
) -> impl Fn(ESpan<'a>) -> IResult<'a, X> {
    ws_before(ws_after(f))
}

/// Version of `separated_nonempty_list` combinator that matches optional whitespace between each
/// item.
fn ws_separated_nonempty_list<
    'a,
    FS: Fn(ESpan<'a>) -> IResult<'a, Y>,
    F: Fn(ESpan<'a>) -> IResult<'a, X>,
    X,
    Y,
>(
    fs: FS,
    f: F,
) -> impl Fn(ESpan<'a>) -> IResult<'a, Vec<X>> {
    separated_nonempty_list(ws_around(fs), f)
}

/// Left-associative operator parsing.
fn lassoc<
    'a: 'b,
    'b,
    F: Fn(ESpan<'a>) -> IResult<'a, X> + 'b,
    FM: Fn(Y, Y) -> X + 'b,
    X: 'b,
    Y: From<X>,
>(
    f: F,
    oper: &'b str,
    mapper: FM,
) -> impl Fn(ESpan<'a>) -> IResult<'a, X> + 'b {
    map(ws_separated_nonempty_list(tag(oper), f), move |items| {
        let mut items = items.into_iter();
        let first = items.next().unwrap();
        items.fold(first, |a, b| mapper(Y::from(a), Y::from(b)))
    })
}

/// Left-associative operator parsing where the operator may take many forms.
fn lassoc_choice<
    'a: 'b,
    'b,
    F: Fn(ESpan<'a>) -> IResult<'a, X> + 'b + Copy,
    FO: Fn(ESpan<'a>) -> IResult<'a, Y> + 'b,
    FM: Fn(X, Y, X) -> X + 'b,
    X: 'b,
    Y: 'b,
>(
    f: F,
    oper: FO,
    mapper: FM,
) -> impl Fn(ESpan<'a>) -> IResult<'a, X> + 'b {
    map(pair(f, many0(pair(ws_around(oper), f))), move |(first, items)| {
        items.into_iter().fold(first, |a, (op, b)| mapper(a, op, b))
    })
}

const KEYWORDS: [&str; 8] = ["let", "const", "def", "if", "else", "true", "false", "null"];

/// Match a keyword.
fn kw<'s>(kw: &'s str) -> impl for<'a> Fn(ESpan<'a>) -> IResult<'a, ESpan<'a>> + 's {
    debug_assert!(KEYWORDS.contains(&kw));
    move |input| {
        terminated(tag(kw), alt((not(alt((alphanumeric1, tag("_")))), err_note("Expected space"))))(
            input,
        )
    }
}

/// We define Whitespace as follows:
///
/// ```
/// ⊔ ← '#' (!<nl> .)* <nl> / AnyUnicodeWhitespace+
/// ```
///
/// Where `AnyUnicodeWhitespace` is any single character classified as whitespace by the Unicode
/// standard.
///
/// Note that our comment syntax is embedded in our whitespace definition:
///
/// ```
/// # This line will parse entirely as whitespace.
/// ```
fn whitespace<'a>(input: ESpan<'a>) -> IResult<'a, ESpan<'a>> {
    recognize(many1(alt((
        take_while1(char::is_whitespace),
        recognize(tuple((chr('#'), many_till(anychar, chr('\n'))))),
    ))))(input)
}

/// Unescaped Identifiers are defined as follows:
///
/// ```
/// UnescapedIdentifier ← [a-zA-Z0-9_]+
/// ```
///
/// Valid unescaped identifiers might include:
///
/// ```
/// 0
/// 3_bean_salad
/// foo
/// item_0
/// a_Mixed_Bag
/// ```
fn unescaped_identifier<'a>(input: ESpan<'a>) -> IResult<'a, ESpan<'a>> {
    recognize(many1(alt((alphanumeric1, tag("_")))))(input)
}

/// Identifiers are defined as follows:
///
/// ```
/// Identifier ← ![0-9] UnescapedIdentifier
/// ```
///
/// Valid identifiers might include:
///
/// ```
/// foo
/// item_0
/// a_Mixed_Bag
/// ```
fn identifier<'a>(input: ESpan<'a>) -> IResult<'a, ESpan<'a>> {
    verify(preceded(not(digit1), unescaped_identifier), |x| !KEYWORDS.contains(x.fragment()))(input)
}

/// Integers are defined as follows:
///
/// ```
/// Digit ← [0-9]
/// HexDigit ← [a-fA-F0-9]
/// DecimalInteger ← '0' !Digit / !'0' Digit+ ( '_' Digit+ )*
/// HexInteger ← '0x' HexDigit+ ( '_' HexDigit+ )*
/// Integer ← DecimalInteger / HexInteger
/// ```
///
/// Valid integers might include:
///
/// ```
/// 0
/// 12345
/// 12_345
/// 0x1234abcd
/// 0x12_abcd
/// ```
fn integer<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(
        alt((
            preceded(not(chr('0')), recognize(separated_nonempty_list(chr('_'), digit1))),
            recognize(tuple((tag("0x"), separated_nonempty_list(chr('_'), hex_digit1)))),
            terminated(tag("0"), not(digit1)),
        )),
        |x: ESpan<'a>| Node::Integer(x.strip_parse_state()),
    )(input)
}

/// Reals are defined as follows:
///
/// ```
/// Real ← Integer '.' Digit+ ( '_' Digit+ )*
/// ```
///
/// Reals look like:
///
/// ```
/// 3.14
/// 12_345.67
/// 1_2_3.45_6
/// ```
fn real<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(recognize(tuple((integer, chr('.'), digit1, many0(pair(chr('_'), digit1))))), |x| {
        Node::Real(x.strip_parse_state())
    })(input)
}

/// Strings are defined as follows:
///
/// ```
/// EscapeSequence ← '\n' / '\t' / '\r' / '\' <nl> / '\\' / '\"' / '\u' HexDigit{6}
/// StringEntity ← !( '\' / '"' / <nl> ) . / EscapeSequence
/// NormalString ← '"' StringEntity* '"'
/// String ← NormalString / MultiString
/// ```
///
/// TODO: Define `MultiString`
///
/// Valid strings might include:
///
/// ```
/// "The quick brown fox jumped over the lazy dog."
/// "A newline.\nA tab\tA code point\u00264b"
/// "String starts here \
/// and keeps on going"
/// ```
fn string<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    normal_string(input)
}

/// See `string`
fn normal_string<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    fn escape_sequence<'a>(input: ESpan<'a>) -> IResult<'a, ESpan<'a>> {
        alt((
            tag(r"\n"),
            tag(r"\t"),
            tag(r"\r"),
            tag("\\\n"),
            tag(r#"\\"#),
            tag(r#"\""#),
            recognize(tuple((tag(r"\u"), map_parser(take(6usize), all_consuming(hex_digit1))))),
            recognize(pair(tag(r"\u"), err_note("'\\u' followed by invalid hex value"))),
            recognize(pair(chr('\\'), err_skip("Bad escape sequence: '\\{}'", anychar))),
            recognize(pair(chr('\\'), err_note("Escape sequence at end of input"))),
        ))(input)
    }

    map(
        recognize(tuple((
            chr('"'),
            many0(alt((recognize(none_of("\\\"\n")), escape_sequence))),
            ex_tag("\""),
        ))),
        |x| Node::String(x.strip_parse_state()),
    )(input)
}

/// Variable declarations are defined as follows:
///
/// ```
/// KWVar ← 'let' !( IdentifierCharacter / '$' )
/// KWConst ← 'const' !( IdentifierCharacter / '$' )
/// VariableDecl ←⊔ ( KWVar / KWConst ) ( Identifier / '$' UnescapedIdentifier ) '=' Expression
/// ```
///
/// Valid variable declarations might include:
///
/// ```
/// let foo = 4
/// const foo = "Ham Sandwich"
/// let $0 = "An Zer0e"
/// ```
fn variable_decl<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(
        tuple((
            alt((kw("let"), kw("const"))),
            ws_around(alt((identifier, preceded(ex_tag("$"), unescaped_identifier)))),
            chr('='),
            ws_before(expression),
        )),
        |(keyword, identifier, _, value)| Node::VariableDecl {
            identifier: identifier.strip_parse_state(),
            value: Box::new(value),
            mutability: if *keyword.fragment() == "const" {
                Mutability::Constant
            } else {
                Mutability::Mutable
            },
        },
    )(input)
}

/// Parameter Lists are defined as follows
///
/// ```
/// OptionalParameter ← Identifier '?'
/// Variadic ← Identifier '..'
/// ParameterList ←⊔ ( Identifier ![.?] )* OptionalParameter* Variadic?
/// ```
///
/// Valid parameter lists might include:
///
/// ```
/// a b
/// a b..
/// a b?
/// a b? c..
/// ```
fn parameter_list<'a>(input: ESpan<'a>) -> IResult<'a, ParameterList<'a>> {
    map(
        tuple((
            separated_list(whitespace, terminated(identifier, not(one_of(".?")))),
            many0(ws_before(terminated(identifier, chr('?')))),
            opt(ws_before(terminated(identifier, tag("..")))),
        )),
        |(parameters, optional_parameters, variadic)| ParameterList {
            parameters: parameters.into_iter().map(StripParseState::strip_parse_state).collect(),
            optional_parameters: optional_parameters
                .into_iter()
                .map(StripParseState::strip_parse_state)
                .collect(),
            variadic: variadic.map(StripParseState::strip_parse_state),
        },
    )(input)
}

/// Function declarations are defined as follows:
///
/// ```
/// FunctionDecl ←⊔ 'def' Identifier ParameterList Block
/// ```
///
/// Valid function declarations might include:
///
/// ```
/// def dothing (c d) { do_other $c; $d * 7 }
/// def variadic a b.. { print $a; print_list $b }
/// def optional a b? { print $a; print_maybe_null $b }
/// def optional_variadic a b? c.. { etc a b c }
/// ```
fn function_decl<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(
        preceded(
            kw("def"),
            tuple((
                ws_before(identifier),
                delimited(opt(tag("(")), ws_around(parameter_list), opt(tag(")"))),
                block,
            )),
        ),
        |(identifier, parameters, body)| Node::FunctionDecl {
            identifier: identifier.strip_parse_state(),
            parameters,
            body: Box::new(body.into()),
        },
    )(input)
}

/// Short function declarations are defined as follows:
///
/// ```
/// ShortFunctionDecl ←⊔ 'def' Identifier '(' ParameterList ')' Expression
/// ```
///
/// Valid function declarations might include:
///
/// ```
/// def frob (a b)  $a + $b
/// def jim () cmd arg argtwo ;
/// def wembly (a..) cmd &
/// def variadic (a b..) { print $a; print_list $b }
/// ```
fn short_function_decl<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(
        preceded(
            kw("def"),
            tuple((
                ws_before(identifier),
                delimited(ws_around(tag("(")), parameter_list, ws_around(tag(")"))),
                expression,
            )),
        ),
        |(identifier, parameters, body)| Node::FunctionDecl {
            identifier: identifier.strip_parse_state(),
            parameters,
            body: Box::new(body),
        },
    )(input)
}

/// Object literals are defined as follows:
///
/// ```
/// Object ←⊔ ObjectLabel? '{' ObjectBody? '}'
/// ObjectLabel ← '@' Identifier
/// ObjectBody ←⊔ Field ( ',' Field  )* ','?
/// Field ←⊔ ( NormalString / Identifier  ) ':' SimpleExpression
/// ```
///
/// Valid object literals might include:
///
/// ```
/// {}
/// { foo: 6, "bar & grill": "Open now"  }
/// { foo: { bar: 6  }, "bar & grill": "Open now"  }
/// @Labeled { foo: 5 }
/// ```
fn object<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    fn field<'a>(input: ESpan<'a>) -> IResult<'a, (Node<'a>, Node<'a>)> {
        separated_pair(
            alt((normal_string, map(identifier, |x| Node::Identifier(x.strip_parse_state())))),
            ws_around(alt((
                tag(":"),
                recognize(err_skip(
                    "Expected ':'",
                    recognize(many0(preceded(
                        not(alt((
                            recognize(identifier),
                            recognize(chr('$')),
                            recognize(chr('}')),
                            whitespace,
                        ))),
                        anychar,
                    ))),
                )),
            ))),
            map(simple_expression, Node::from),
        )(input)
    }

    fn object_body<'a>(input: ESpan<'a>) -> IResult<'a, Vec<(Node<'a>, Node<'a>)>> {
        map(
            opt(terminated(
                flat_map(field, |node| {
                    fold_many0(
                        preceded(ws_around(chr(',')), field),
                        Rc::new(RefCell::new(vec![node])),
                        |acc, item| {
                            acc.borrow_mut().push(item);
                            acc
                        },
                    )
                }),
                opt(ws_before(ex_tag(","))),
            )),
            |x| x.map(|y| y.take()).unwrap_or_else(Vec::new),
        )(input)
    }

    map(
        pair(
            opt(ws_after(preceded(chr('@'), identifier))),
            delimited(chr('{'), ws_around(object_body), ex_tag("}")),
        ),
        |(name, body)| Node::Object(name.map(|x| x.strip_parse_state()), body),
    )(input)
}

/// List literals are defined as follows:
///
/// ```
/// List ←⊔ '[' ListBody? ']'
/// ListBody ←⊔ SimpleExpression ( ',' SimpleExpression  )* ','?
/// ```
///
/// Valid list literals might include:
///
/// ```
/// []
/// [ 6, "Open now"  ]
/// [ { bar: 6 }, "Open now"  ]
/// ```
fn list<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    fn list_body<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
        map(
            opt(terminated(
                flat_map(simple_expression, |node| {
                    let node = node.into();
                    fold_many0(
                        preceded(ws_around(chr(',')), simple_expression),
                        Rc::new(RefCell::new(vec![node])),
                        |acc, item| {
                            acc.borrow_mut().push(item.into());
                            acc
                        },
                    )
                }),
                opt(ws_before(ex_tag(","))),
            )),
            |x| Node::List(x.map(|x| x.take()).unwrap_or_else(Vec::new)),
        )(input)
    }

    delimited(chr('['), ws_around(list_body), ex_tag("]"))(input)
}

/// Lambda expressions are defined as follows:
///
/// ```
/// Lambda ←⊔ '\' Identifier Invocation
/// Lambda ←⊔ '\(' ParameterList ')' Invocation
/// ```
///
/// Lambdas look like:
///
/// ```
/// \a  $a * 2
/// \(a b) $a + $b
/// \() doThing $arg $arg
/// \a { let s = 2; $a + $s }
/// ```
fn lambda<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(
        pair(
            alt((
                delimited(tag("\\("), ws_before(parameter_list), ws_around(tag(")"))),
                map(preceded(tag("\\"), ws_around(identifier)), |parameter| ParameterList {
                    parameters: vec![parameter.strip_parse_state()],
                    optional_parameters: Vec::new(),
                    variadic: None,
                }),
            )),
            invocation,
        ),
        |(parameters, body)| Node::Lambda { parameters, body: Box::new(body.into()) },
    )(input)
}

/// Lookups are defined as follows:
///
/// ```
/// Lookup ←⊔ Value ( '.' Identifier / '[' Expression ']')*
/// ```
///
/// It looks like this:
///
/// ```
/// $foo.bar
/// $foo["Bar"]
/// $foo[6 + 7]
/// ```
fn lookup<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(
        pair(
            value,
            many0(alt((
                preceded(
                    ws_around(tag(".")),
                    map(identifier, |x| Node::Label(x.strip_parse_state())),
                ),
                delimited(chr('['), ws_around(expression), chr(']')),
            ))),
        ),
        |(x, y)| {
            y.into_iter()
                .fold(x, |prev, ident| Node::Lookup(Box::new(prev.into()), Box::new(ident)).into())
        },
    )(input)
}

/// Parentheticals are defined as:
///
/// ```
/// Parenthetical ←⊔ '(' Expression ')'
/// ```
///
/// They look like this:
///
/// ```
/// 2 * ( 2 + 3 )
/// ```
fn parenthetical<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    delimited(chr('('), ws_around(expression), chr(')'))(input)
}

/// Blocks are defined as:
///
/// ```
/// Block ←⊔ '{' Program '}'
/// ```
///
/// They look like this:
///
/// ```
/// { let s = 2; 2 + 2; }
/// ```
fn block<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(delimited(chr('{'), ws_around(program), chr('}')), Node::Block)(input)
}

/// Values are defined as follows:
///
/// ```
/// Value ← List / Object / Lambda / Parenthetical / Block / If / Atom
/// Atom ← String / Real / Integer / '$' UnescapedIdentifier
///
/// ```
fn value<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    alt((
        list,
        object,
        lambda,
        parenthetical,
        block,
        conditional,
        map(preceded(ex_tag("$"), unescaped_identifier), |x| {
            Node::Identifier(x.strip_parse_state())
        }),
        map(kw("true"), |_| Node::True),
        map(kw("false"), |_| Node::False),
        map(kw("null"), |_| Node::Null),
        string,
        real,
        integer,
        err_insert("Expected value"),
    ))(input)
}

/// Range literals are defined as follows:
///
/// ```
/// Range ←⊔ LogicalOr? ( '..=' / '..' ) LogicalOr? / LogicalOr
/// ```
///
/// Range literals look like so:
///
/// ```
/// 1..2
/// $a .. $a + 2
/// $a ..= $a + 2
/// $a ..
/// .. $b
/// ..
/// ```
fn range<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    let oper = preceded(tag(".."), map(opt(chr('=')), |x| x.is_some()));

    alt((
        map(tuple((opt(logical_or), ws_around(oper), opt(logical_or))), |(a, is_inclusive, b)| {
            Node::Range(Box::new(a.map(Node::from)), Box::new(b.map(Node::from)), is_inclusive)
                .into()
        }),
        logical_or,
    ))(input)
}

/// Logical "and" and "or" are defined as follows:
///
/// ```
/// LogicalAnd ←⊔ LogicalNot ( '&&' LogicalNot )*
/// LogicalOr ←⊔ LogicalAnd ( '||' LogicalAnd )*
/// ```
///
/// It looks like this:
///
/// ```
/// $a && $b || $c && $d || $e && $f
/// ```
fn logical_or<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    lassoc(logical_and, "||", |x: Node<'a>, y| {
        Node::LogicalOr(Box::new(x.into()), Box::new(y.into()))
    })(input)
}

/// See `logical_or`
fn logical_and<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    lassoc(logical_not, "&&", |x: Node<'a>, y| Node::LogicalAnd(Box::new(x), Box::new(y)))(input)
}

/// Logical negation is defined as follows:
///
/// ```
/// LogicalNot ←⊔ '!'* Comparison
/// ```
///
/// It looks like this:
///
/// ```
/// !$a
/// !!$b
/// ```
fn logical_not<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(pair(many0_count(ws_after(chr('!'))), comparison), |(count, node)| {
        (0..count).fold(node, |x, _| Node::LogicalNot(Box::new(x.into())))
    })(input)
}

/// Comparisons are defined as follows:
///
/// ```
/// CompOp ← '<=' / '<' / '>=' / '>' / '!=' / '=='
/// Comparison ←⊔ AddSub ( CompOp AddSub )*
/// ```
///
/// They look like this:
///
/// ```
/// $a > $b
/// $a < $b
/// $a <= $b
/// $a == $b
/// ```
fn comparison<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    fn comp_op<'a>(input: ESpan<'a>) -> IResult<'a, ESpan<'a>> {
        terminated(
            alt((tag("<="), tag("<"), tag(">="), tag(">"), tag("!="), tag("=="))),
            opt(ws_before(err_skip("Logical not can't occur here", chr('!')))),
        )(input)
    }

    lassoc_choice(add_subtract, comp_op, |a, op, b| {
        match *op.fragment() {
            "<=" => Node::LE(Box::new(a), Box::new(b)),
            "<" => Node::LT(Box::new(a), Box::new(b)),
            ">=" => Node::GE(Box::new(a), Box::new(b)),
            ">" => Node::GT(Box::new(a), Box::new(b)),
            "!=" => Node::NE(Box::new(a), Box::new(b)),
            "==" => Node::EQ(Box::new(a), Box::new(b)),
            _ => unreachable!(),
        }
        .into()
    })(input)
}

/// Addition is defined as follows:
///
/// ```
/// AddSub ←⊔ MulDiv ( [+-] MulDiv )*
/// ```
///
/// It looks as you'd expect:
///
/// ```
/// $a + $b
/// $a - $b
/// ```
fn add_subtract<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    lassoc_choice(multiply_divide, one_of("+-"), |a, op, b| match op {
        '-' => Node::Subtract(Box::new(a.into()), Box::new(b.into())),
        '+' => Node::Add(Box::new(a.into()), Box::new(b.into())),
        _ => unreachable!(),
    })(input)
}

/// Multiplication/division is defined as follows:
///
/// ```
/// MulDiv ←⊔ Negate ( ( '*' / '//' ) Negate )*
/// ```
///
/// It looks like this:
///
/// ```
/// $a * $b
/// $a // $b
/// ```
fn multiply_divide<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    lassoc_choice(negate, alt((tag("*"), tag("//"))), |a, op, b| match *op.fragment() {
        "*" => Node::Multiply(Box::new(a.into()), Box::new(b.into())),
        "//" => Node::Divide(Box::new(a.into()), Box::new(b.into())),
        _ => unreachable!(),
    })(input)
}

/// Arithmetic negation is defined as follows:
///
/// ```
/// Negate ←⊔ '-' Negate / Lookup
/// ```
///
/// It looks like this:
///
/// ```
/// -$a
/// ```
fn negate<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(pair(many0_count(ws_after(chr('-'))), lookup), |(count, node)| {
        (0..count).fold(node, |x, _| Node::Negate(Box::new(x)))
    })(input)
}

/// Invocation is defined as:
///
/// ```
/// BareString ← ( !⊔ . )+
/// Invocation ←⊔ Identifier ( SimpleExpression / BareString )* / SimpleExpression
/// ```
///
/// Invocation looks like:
///
/// ```
/// a $b $c
/// foo.bar 16 -7
/// do_thing --my_arg 3
/// ```
fn invocation<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    fn bare_string<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
        map(
            recognize(many1(preceded(
                not(alt((recognize(whitespace), recognize(one_of("@{}|&;()"))))),
                anychar,
            ))),
            |x| Node::BareString(x.strip_parse_state()),
        )(input)
    }

    alt((
        map(
            pair(
                identifier,
                many0(ws_before(alt((
                    delimited(
                        not(tag(".")),
                        simple_expression,
                        peek(alt((
                            recognize(whitespace),
                            recognize(one_of("|&;)}")),
                            recognize(not(anychar)),
                        ))),
                    ),
                    bare_string,
                )))),
            ),
            |(first, args)| {
                Node::Invocation(
                    first.strip_parse_state(),
                    args.into_iter().map(Node::from).collect(),
                )
            },
        ),
        map(simple_expression, Node::from),
    ))(input)
}

/// Assignment is defined as:
///
/// ```
/// Assignment ←⊔ ( SimpleExpression '=' )* Invocation
/// ```
///
/// Assignment looks like:
///
/// ```
/// $a = $b
/// $a = $b = $c
/// ```
fn assignment<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(pair(many0(terminated(simple_expression, ws_around(tag("=")))), invocation), |(a, b)| {
        a.into_iter().rev().fold(b, |b, a| Node::Assignment(Box::new(a.into()), Box::new(b)))
    })(input)
}

/// Conditionals are defined as:
///
/// ```
/// If ←⊔ 'if' SimpleExpression Block ( 'else' ( If / Block ) )?
/// ```
///
/// Conditionals look like:
///
/// ```
/// if $a { b }
/// if $a { b } else { c }
/// if $a { b } else if $c { d } else { e }
/// ```
fn conditional<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    map(
        tuple((
            kw("if"),
            ws_before(simple_expression),
            ws_before(block),
            opt(preceded(ws_around(kw("else")), alt((conditional, block)))),
        )),
        |(_, condition, body, else_)| Node::If {
            condition: Box::new(condition.into()),
            body: Box::new(body),
            else_: else_.map(Box::new),
        },
    )(input)
}

/// Expressions are defined as follows:
///
/// ```
/// SimpleExpression ← Range
/// Expression ←⊔ ( Assignment ( '|>' / '|' ) )* Assignment
/// ```
///
/// Expressions look like:
///
/// ```
/// abc | def |> ghi
/// ```
fn expression<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    alt((
        lassoc_choice(assignment, alt((tag("|>"), tag("|"))), |a, op, b| match *op.fragment() {
            "|>" => Node::Iterate(Box::new(a), Box::new(b)),
            "|" => Node::Pipe(Box::new(a), Box::new(b)),
            _ => unreachable!(),
        }),
        err_insert("Expected expression"),
    ))(input)
}

/// See `expression`
fn simple_expression<'a>(input: ESpan<'a>) -> IResult<'a, Node<'a>> {
    range(input)
}

/// A program is defined as:
///
/// ```
/// Program ←⊔ ( ( FunctionDecl Program? ) / ( ( VariableDecl / ShortFunctionDecl / Expression ) ( [;&] Program )? ) )?
/// ```
fn program<'a>(input: ESpan<'a>) -> IResult<'a, Vec<Node<'a>>> {
    let mut input_next = input;
    let mut vec = Vec::new();

    while let Ok((tail, (node, terminator))) = preceded(
        cond(!vec.is_empty(), opt(whitespace)),
        alt((
            pair(function_decl, map(opt(ws_before(one_of(";&"))), |x| x.or(Some(';')))),
            pair(
                alt((variable_decl, short_function_decl, expression)),
                opt(ws_before(one_of(";&"))),
            ),
        )),
    )(input_next.clone())
    {
        let node = if let Some('&') = terminator {
            if let Node::FunctionDecl { identifier, parameters, body } = node {
                Node::FunctionDecl { identifier, parameters, body: Box::new(Node::Async(body)) }
            } else {
                Node::Async(Box::new(node))
            }
        } else {
            node
        };

        input_next = tail;
        vec.push(node);
        if terminator.is_none() {
            break;
        }
    }

    Ok((input_next, vec))
}

#[cfg(test)]
mod test {
    use super::*;

    /// Quick helper for testing parsing.
    fn test_parse_err(
        text: &str,
        expected: Node<'_>,
        expected_errors: Vec<(usize, &'_ str, &'_ str)>,
    ) {
        let result = ParseResult::from(text);
        let actual = result.tree;
        let mut errors = result.errors;
        assert!(
            actual.content_eq(&expected),
            "Unexpected result\nexpected: {:#?}\ngot     : {:#?}",
            expected,
            actual
        );

        for expected_error in expected_errors {
            if let Some((idx, _)) = errors.iter().enumerate().find(|(_, (e, m))| {
                *m == expected_error.1
                    && e.location_offset() == expected_error.0
                    && *e.fragment() == expected_error.2
            }) {
                errors.remove(idx);
            } else {
                panic!("Missing error: {expected_error:?}");
            }
        }

        assert!(errors.is_empty(), "Unexpected errors: {errors:#?}");
    }

    /// Quick helper for testing parsing.
    fn test_parse(text: &str, expected: Node<'_>) {
        test_parse_err(text, expected, Vec::new())
    }

    /// Shorthand for `Span::new`
    fn sp(s: &str) -> Span<'_> {
        Span::new(s)
    }

    /// Quick constructor for an identifier node.
    fn ident(s: &str) -> Node<'_> {
        Node::Identifier(sp(s))
    }

    /// Quick constructor for a string literal node.
    fn string(s: &str) -> Node<'_> {
        Node::String(sp(s))
    }

    #[test]
    fn variable_decl() {
        test_parse(
            "let s = 0",
            Node::Program(vec![Node::VariableDecl {
                identifier: sp("s"),
                value: Box::new(Node::Integer(sp("0"))),
                mutability: Mutability::Mutable,
            }]),
        );
    }

    #[test]
    fn labeled_object_one_field() {
        test_parse(
            r#"@Foo { bar: "baz" }"#,
            Node::Program(vec![Node::Object(
                Some(sp("Foo")),
                vec![(ident("bar"), string(r#""baz""#))],
            )]),
        );
    }

    #[test]
    fn labeled_empty_object() {
        test_parse(r#"@Foo {}"#, Node::Program(vec![Node::Object(Some(sp("Foo")), vec![])]));
    }

    #[test]
    fn object_field_specified_with_identifier_missing_sigil() {
        test_parse_err(
            r#"@Foo { bar: a }"#,
            Node::Program(vec![Node::Object(Some(sp("Foo")), vec![(ident("bar"), ident("a"))])]),
            vec![(12, "Expected '$'", "")],
        );
    }

    #[test]
    fn object_field_specified_with_identifier_field_has_equals() {
        test_parse_err(
            r#"@Foo { bar = $a }"#,
            Node::Program(vec![Node::Object(Some(sp("Foo")), vec![(ident("bar"), ident("a"))])]),
            vec![(11, "Expected ':'", "=")],
        );
    }

    #[test]
    fn function_decl() {
        test_parse(
            r#"def foo() bar"#,
            Node::Program(vec![Node::FunctionDecl {
                identifier: sp("foo"),
                parameters: ParameterList {
                    parameters: vec![],
                    optional_parameters: vec![],
                    variadic: None,
                },
                body: Box::new(Node::Invocation(sp("bar"), vec![])),
            }]),
        );
    }

    #[test]
    fn function_decl_block() {
        test_parse(
            r#"def foo { bar }"#,
            Node::Program(vec![Node::FunctionDecl {
                identifier: sp("foo"),
                parameters: ParameterList {
                    parameters: vec![],
                    optional_parameters: vec![],
                    variadic: None,
                },
                body: Box::new(Node::Block(vec![Node::Invocation(sp("bar"), vec![])])),
            }]),
        );
    }

    #[test]
    fn function_decl_block_terminated() {
        test_parse(
            r#"def foo { bar };"#,
            Node::Program(vec![Node::FunctionDecl {
                identifier: sp("foo"),
                parameters: ParameterList {
                    parameters: vec![],
                    optional_parameters: vec![],
                    variadic: None,
                },
                body: Box::new(Node::Block(vec![Node::Invocation(sp("bar"), vec![])])),
            }]),
        );
    }

    #[test]
    fn function_decl_block_async() {
        test_parse(
            r#"def foo { bar }&"#,
            Node::Program(vec![Node::FunctionDecl {
                identifier: sp("foo"),
                parameters: ParameterList {
                    parameters: vec![],
                    optional_parameters: vec![],
                    variadic: None,
                },
                body: Box::new(Node::Async(Box::new(Node::Block(vec![Node::Invocation(
                    sp("bar"),
                    vec![],
                )])))),
            }]),
        );
    }

    #[test]
    fn function_decl_args() {
        test_parse(
            r#"def foo (a) bar"#,
            Node::Program(vec![Node::FunctionDecl {
                identifier: sp("foo"),
                parameters: ParameterList {
                    parameters: vec![sp("a")],
                    optional_parameters: vec![],
                    variadic: None,
                },
                body: Box::new(Node::Invocation(sp("bar"), vec![])),
            }]),
        );
    }

    #[test]
    fn function_decl_opt_args() {
        test_parse(
            r#"def foo (a?) bar"#,
            Node::Program(vec![Node::FunctionDecl {
                identifier: sp("foo"),
                parameters: ParameterList {
                    parameters: vec![],
                    optional_parameters: vec![sp("a")],
                    variadic: None,
                },
                body: Box::new(Node::Invocation(sp("bar"), vec![])),
            }]),
        );
    }

    #[test]
    fn function_decl_variadic_arg() {
        test_parse(
            r#"def foo (a..) bar"#,
            Node::Program(vec![Node::FunctionDecl {
                identifier: sp("foo"),
                parameters: ParameterList {
                    parameters: vec![],
                    optional_parameters: vec![],
                    variadic: Some(sp("a")),
                },
                body: Box::new(Node::Invocation(sp("bar"), vec![])),
            }]),
        );
    }

    #[test]
    fn function_decl_all_arg() {
        test_parse(
            r#"def foo (a b? c..) bar"#,
            Node::Program(vec![Node::FunctionDecl {
                identifier: sp("foo"),
                parameters: ParameterList {
                    parameters: vec![sp("a")],
                    optional_parameters: vec![sp("b")],
                    variadic: Some(sp("c")),
                },
                body: Box::new(Node::Invocation(sp("bar"), vec![])),
            }]),
        );
    }

    #[test]
    fn lambda() {
        test_parse(
            r#"\foo bar"#,
            Node::Program(vec![Node::Lambda {
                parameters: ParameterList {
                    parameters: vec![sp("foo")],
                    optional_parameters: vec![],
                    variadic: None,
                },
                body: Box::new(Node::Invocation(sp("bar"), vec![])),
            }]),
        );
    }

    #[test]
    fn lambda_fancy_args() {
        test_parse(
            r#"\(a b? c..) bar"#,
            Node::Program(vec![Node::Lambda {
                parameters: ParameterList {
                    parameters: vec![sp("a")],
                    optional_parameters: vec![sp("b")],
                    variadic: Some(sp("c")),
                },
                body: Box::new(Node::Invocation(sp("bar"), vec![])),
            }]),
        );
    }

    #[test]
    fn list() {
        test_parse(
            r#"[1, 2, 3]"#,
            Node::Program(vec![Node::List(vec![
                Node::Integer(sp("1")),
                Node::Integer(sp("2")),
                Node::Integer(sp("3")),
            ])]),
        );
    }

    #[test]
    fn fancy_integers() {
        test_parse(
            r#"[1_234_5, 0x2abcd, 3]"#,
            Node::Program(vec![Node::List(vec![
                Node::Integer(sp("1_234_5")),
                Node::Integer(sp("0x2abcd")),
                Node::Integer(sp("3")),
            ])]),
        );
    }

    #[test]
    fn reals() {
        test_parse(
            r#"[3.14, 1_23.4_5, 3_4.5_6]"#,
            Node::Program(vec![Node::List(vec![
                Node::Real(sp("3.14")),
                Node::Real(sp("1_23.4_5")),
                Node::Real(sp("3_4.5_6")),
            ])]),
        );
    }

    #[test]
    fn string_test() {
        test_parse(
            r#""straang\t\r\n\
\\abcd\u00264b\"""#,
            Node::Program(vec![Node::String(sp("\"straang\\t\\r\\n\\\n\\\\abcd\\u00264b\\\"\""))]),
        );
    }

    #[test]
    fn pipes() {
        test_parse(
            r#"a | b |> c | d |> e"#,
            Node::Program(vec![Node::Iterate(
                Box::new(Node::Pipe(
                    Box::new(Node::Iterate(
                        Box::new(Node::Pipe(
                            Box::new(Node::Invocation(sp("a"), vec![])),
                            Box::new(Node::Invocation(sp("b"), vec![])),
                        )),
                        Box::new(Node::Invocation(sp("c"), vec![])),
                    )),
                    Box::new(Node::Invocation(sp("d"), vec![])),
                )),
                Box::new(Node::Invocation(sp("e"), vec![])),
            )]),
        );
    }

    #[test]
    fn paren_pipes() {
        test_parse(
            r#"q (a | b |> c | d |> e) $f"#,
            Node::Program(vec![Node::Invocation(
                sp("q"),
                vec![
                    Node::Iterate(
                        Box::new(Node::Pipe(
                            Box::new(Node::Iterate(
                                Box::new(Node::Pipe(
                                    Box::new(Node::Invocation(sp("a"), vec![])),
                                    Box::new(Node::Invocation(sp("b"), vec![])),
                                )),
                                Box::new(Node::Invocation(sp("c"), vec![])),
                            )),
                            Box::new(Node::Invocation(sp("d"), vec![])),
                        )),
                        Box::new(Node::Invocation(sp("e"), vec![])),
                    ),
                    Node::Identifier(sp("f")),
                ],
            )]),
        );
    }

    #[test]
    fn conditional_test() {
        test_parse(
            r#"if $a { b } else if $q { c } else { e }"#,
            Node::Program(vec![Node::If {
                condition: Box::new(Node::Identifier(sp("a"))),
                body: Box::new(Node::Block(vec![Node::Invocation(sp("b"), vec![])])),
                else_: Some(Box::new(Node::If {
                    condition: Box::new(Node::Identifier(sp("q"))),
                    body: Box::new(Node::Block(vec![Node::Invocation(sp("c"), vec![])])),
                    else_: Some(Box::new(Node::Block(vec![Node::Invocation(sp("e"), vec![])]))),
                })),
            }]),
        );
    }

    #[test]
    fn bare_string_starts_with_numbers() {
        test_parse(
            r#"foo 123abc"#,
            Node::Program(vec![Node::Invocation(sp("foo"), vec![Node::BareString(sp("123abc"))])]),
        );
    }

    #[test]
    fn args_that_start_with_dot_are_strings() {
        test_parse(
            r#"cd .."#,
            Node::Program(vec![Node::Invocation(sp("cd"), vec![Node::BareString(sp(".."))])]),
        );
    }
}
