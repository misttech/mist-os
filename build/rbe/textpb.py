#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generic utilities for working with text protobufs (without schema).
"""

import enum
import collections
import dataclasses
import re

from typing import Any, Dict, Iterable, Sequence


class TokenType(enum.Enum):
    FIELD_NAME = 0  # includes trailing ':'
    START_BLOCK = 1  # '<' or '{'
    END_BLOCK = 2  # '>' or '}'
    STRING_VALUE = 3  # quoted text, e.g. "string"
    OTHER_VALUE = 4  # non-string value
    SPACE = 5  # [ \t]*
    NEWLINE = 6  # [\n\r]


@dataclasses.dataclass
class Token(object):
    text: str
    type: TokenType


_FIELD_NAME_RE = re.compile(r'[a-zA-Z_][a-zA-Z0-9_]*:')
_SPACE_RE = re.compile(r'[ \t]+')
_NEWLINE_RE = re.compile(r'\r?\n')
_STRING_RE = re.compile(r'\"[^"]*\"')
_VALUE_RE = re.compile(r'[^ \t\r\n]+')  # Anything text that is not space


def _lex_line(line: str) -> Iterable[Token]:
    prev: Token = None
    while line:  # is not empty
        next_char = line[0]

        if next_char in {'<', '{'}:
            yield Token(text=next_char, type=TokenType.START_BLOCK)
            line = line[1:]
            continue

        if next_char in {'>', '}'}:
            yield Token(text=next_char, type=TokenType.END_BLOCK)
            line = line[1:]
            continue

        field_match = _FIELD_NAME_RE.match(line)
        if field_match:
            field_name = field_match.group(0)
            yield Token(text=field_name, type=TokenType.FIELD_NAME)
            line = line[len(field_name):]
            continue

        string_match = _STRING_RE.match(line)
        if string_match:
            string = string_match.group(0)
            yield Token(text=string, type=TokenType.STRING_VALUE)
            line = line[len(string):]
            continue

        value_match = _VALUE_RE.match(line)
        if value_match:
            value = value_match.group(0)
            yield Token(text=value, type=TokenType.OTHER_VALUE)
            line = line[len(value):]
            continue

        space_match = _SPACE_RE.match(line)
        if space_match:
            space = space_match.group(0)
            yield Token(text=space, type=TokenType.SPACE)
            line = line[len(space):]
            continue

        newline_match = _NEWLINE_RE.match(line)
        if newline_match:
            newline = newline_match.group(0)
            yield Token(text=newline, type=TokenType.NEWLINE)
            line = line[len(newline):]
            continue

        raise ValueError(f'[textpb.lex] Unrecognized text: "{line}"')


def _lex(lines: Iterable[str]) -> Iterable[Token]:
    """Divides proto text into tokens."""
    for line in lines:
        yield from _lex_line(line)

class ParseError(ValueError):

    def __init__(self, msg: str):
        super().__init__(msg)


def _auto_dict(values: Sequence[Any]):
    """Convert sequences of key-value pairs to dictionaries."""
    if len(values) == 0:
      return values

    if all(isinstance(v, dict) and v.keys() == {'key', 'value'} for v in values):
      # assume keys are unique quoted strings
      # 'key' and 'value' should not be repeated fields
      return {v['key'][0].text.strip('"'): v['value'][0] for v in values}

    return values


def _parse_block(tokens: Iterable[Token],
                 top: bool) -> Dict[str, Sequence[Any]]:
    """Parse text proto tokens into a structure.

    Args:
      tokens: lexical tokens, without any spaces/newlines.

    Returns:
      dictionary representation of text proto.
    """
    # Without a schema, we cannot deduce whether a field is scalar or
    # repeated, so treat them as repeated (maybe singleton).
    result = collections.defaultdict(list)

    while True:
        try:
            field = next(tokens)
        except StopIteration:
            if top:
                break
            else:
                raise ParseError(
                    "Unexpected EOF, missing '>' or '}' end-of-block")

        if field.type == TokenType.END_BLOCK:
            if top:
                raise ParseError(
                    "Unexpected end-of-block at top-level before EOF.")
            break

        try:
            value_or_block = next(tokens)
        except StopIteration:
            raise ParseError("Unexpected EOF, expecting a value or start-of-block.")

        key = field.text[:-1]
        if value_or_block.type == TokenType.START_BLOCK:
            result[key].append(_parse_block(tokens, top=False))
        elif value_or_block.type in {TokenType.STRING_VALUE,
                                     TokenType.OTHER_VALUE}:
            result[key].append(value_or_block)
        else:
            raise ParseError(f"Unexpected token: {value_or_block.text}")

    # End of block, post-process key-value pairs into dictionaries.
    return {k: _auto_dict(v) for k, v in result.items()}


def _parse_tokens(tokens: Iterable[Token]) -> Dict[str, Sequence[Any]]:
    return _parse_block(tokens, top=True)


def parse(lines: Iterable[str]) -> Dict[str, Sequence[Any]]:
    """Parse a text protobuf into a recursive dictionary.

    Args:
      lines: lines of text proto (spaces and line breaks are ignored)

    Returns:
      Structured representation of the proto.
      Fields are treated as either repeated (even if the original
      schema was scalar) or as key-value dictionaries.
    """
    # ignore spaces
    return _parse_tokens(
        token for token in _lex(lines)
        if token.type not in {TokenType.SPACE, TokenType.NEWLINE})
