#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generic utilities for working with depfiles
"""

import enum
import dataclasses
import re

from typing import Callable, Iterable


class TokenType(enum.Enum):
    PATH = 0  # anything that isn't the others
    COLON = 1  # ':'
    SPACE = 2  # [ \t]
    NEWLINE = 3  # [\n\r]
    LINECONTINUE = 4  # '\\' (also start of escape-sequence)
    COMMENT = 5  # '#...'
    ESCAPED = 6  # e.g. '\t'


@dataclasses.dataclass
class Token(object):
    text: str
    type: TokenType


_SPACE_RE = re.compile(r'[ \t]+')
_NEWLINE_RE = re.compile(r'\r?\n')
_PATH_RE = re.compile(r'[a-zA-Z0-9_/.+-]+')
_COMMENT_RE = re.compile(r'#[^\n]*')


def _lex_line(line: str) -> Iterable[Token]:
    prev : Token = None
    while line:  # is not empty
        next_char = line[0]

        if prev and prev.Type == TokenType.LINECONTINUE:
            newline_match = _NEWLINE_RE.match(line)
            if newline_match:
                newline = newline_match.group(0)
                yield prev
                prev = None
                yield Token(text=newline, type=TokenType.NEWLINE)
                line = line[len(newline):]
                continue

            # otherwise, we don't handle escaped sequences yet
            new = Token(text=prev.text + next_char, type=TokenType.ESCAPED)
            raise ValueError(f'Escape sequences are not handled yet: {new}')

        if next_char == ':':
            yield Token(text=next_char, type=TokenType.COLON)
            line = line[1:]
            continue

        if next_char == '\\':
            prev = Token(text=next_char, type=TokenType.LINECONTINUE)
            # do not yield yet, look at text that follows
            line = line[1:]
            continue

        space_match = _SPACE_RE.match(line)
        if space_match:
            spaces = space_match.group(0)
            yield Token(text=spaces, type=TokenType.SPACE)
            line = line[len(spaces):]
            continue

        newline_match = _NEWLINE_RE.match(line)
        if newline_match:
            newline = newline_match.group(0)
            yield Token(text=newline, type=TokenType.NEWLINE)
            line = line[len(newline):]
            continue

        comment_match = _COMMENT_RE.match(line)
        if comment_match:
            comment = comment_match.group(0)
            yield Token(text=comment, type=TokenType.COMMENT)
            line = line[len(comment):]
            continue

        path_match = _PATH_RE.match(line)
        if path_match:
            path = path_match.group(0)
            yield Token(text=path, type=TokenType.PATH)
            line = line[len(path):]
            continue

        raise ValueError(f'[depfile.lex] Unrecognized text: "{line}"')

    if prev:
        yield prev

def lex(lines: Iterable[str]) -> Iterable[Token]:
    """Divides depfile text into tokens."""
    for line in lines:
        yield from _lex_line(line)


def _transform_paths(tokens: Iterable[Token],
                     transform: Callable[[str], str]) -> Iterable[Token]:
    """Space-preserving path transformation."""
    for token in tokens:
        if token.type == TokenType.PATH:
            yield Token(text=transform(token.text), type=TokenType.PATH)
        else:
            yield token


def unlex(tokens: Iterable[Token]) -> str:
    """Concatenates tokens' text."""
    return ''.join(token.text for token in tokens)


def transform_paths(text: str, transform: Callable[[str], str]) -> str:
    """Applies an arbitrary transformation to depfile paths."""
    return unlex(
        _transform_paths(lex(text.splitlines(keepends=True)), transform))


# TODO(http://fxbug.dev/124714): implement parser, if needed
