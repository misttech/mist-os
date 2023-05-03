#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import sys
import unittest
from pathlib import Path
from unittest import mock
from typing import Sequence

import depfile


class LexLineTests(unittest.TestCase):

    def _test_tokens(self, tokens: Sequence[depfile.Token]):
        text = depfile.unlex(tokens)
        lexed = list(depfile._lex_line(text))
        self.assertEqual(lexed, tokens)

    def test_empty(self):
        self._test_tokens([])

    def test_comment(self):
        self._test_tokens([depfile.Token('#', depfile.TokenType.COMMENT)])

    def test_comment_long(self):
        self._test_tokens(
            [
                depfile.Token(
                    '# DO NOT EDIT, generated', depfile.TokenType.COMMENT)
            ])

    def test_colon(self):
        self._test_tokens([depfile.Token(':', depfile.TokenType.COLON)])

    def test_colons(self):
        self._test_tokens([depfile.Token(':', depfile.TokenType.COLON)] * 2)

    def test_newline(self):
        self._test_tokens([depfile.Token('\n', depfile.TokenType.NEWLINE)])

    def test_newline_return(self):
        self._test_tokens([depfile.Token('\r\n', depfile.TokenType.NEWLINE)])

    def test_newlines(self):
        self._test_tokens([depfile.Token('\n', depfile.TokenType.NEWLINE)] * 2)

    def test_newline_returns(self):
        self._test_tokens(
            [depfile.Token('\r\n', depfile.TokenType.NEWLINE)] * 2)

    def test_space(self):
        self._test_tokens([depfile.Token(' ', depfile.TokenType.SPACE)])

    def test_spaces(self):
        self._test_tokens([depfile.Token('    ', depfile.TokenType.SPACE)])

    def test_tab(self):
        self._test_tokens([depfile.Token('\t', depfile.TokenType.SPACE)])

    def test_tabs(self):
        self._test_tokens([depfile.Token('\t\t', depfile.TokenType.SPACE)])

    def test_spacetabs(self):
        self._test_tokens(
            [depfile.Token('\t   \t  \t\t ', depfile.TokenType.SPACE)])

    def test_line_continue(self):
        self._test_tokens([depfile.Token('\\', depfile.TokenType.LINECONTINUE)])

    def test_path(self):
        for text in ('_', 'a', 'a.txt', 'f_e.g.h', 'READ-ME.md', '/x', '/x/y/z',
                     '/f/g-h.ij', '/usr/include/c++/v1'):
            self._test_tokens([depfile.Token(text, depfile.TokenType.PATH)])

    def test_dep(self):
        self._test_tokens(
            [
                depfile.Token('a', depfile.TokenType.PATH),
                depfile.Token(':', depfile.TokenType.COLON),
                depfile.Token('b.c', depfile.TokenType.PATH),
                depfile.Token('    ', depfile.TokenType.SPACE),
                depfile.Token('d/e', depfile.TokenType.PATH),
                depfile.Token('\n', depfile.TokenType.NEWLINE),
            ])


class LexTests(unittest.TestCase):

    def _test_lines(self, tokens: Sequence[Sequence[depfile.Token]]):
        lines = [depfile.unlex(t) for t in tokens]
        lexed = list(depfile.lex(lines))
        self.assertEqual(lexed, [s for t in tokens for s in t])

    def test_one_dep(self):
        self._test_lines(
            [
                [
                    depfile.Token('x/y.z', depfile.TokenType.PATH),
                    depfile.Token(':', depfile.TokenType.COLON),
                    depfile.Token('bb.c', depfile.TokenType.PATH),
                    depfile.Token('    ', depfile.TokenType.SPACE),
                    depfile.Token('/d/e', depfile.TokenType.PATH),
                    depfile.Token('\n', depfile.TokenType.NEWLINE),
                ]
            ])

    def test_two_deps(self):
        self._test_lines(
            [
                [
                    depfile.Token('x/y.z', depfile.TokenType.PATH),
                    depfile.Token(':', depfile.TokenType.COLON),
                    depfile.Token(' ', depfile.TokenType.SPACE),
                    depfile.Token('bb.c', depfile.TokenType.PATH),
                    depfile.Token('\n', depfile.TokenType.NEWLINE),
                ],
                [
                    depfile.Token('p/q/r.s', depfile.TokenType.PATH),
                    depfile.Token(' ', depfile.TokenType.SPACE),
                    depfile.Token(':', depfile.TokenType.COLON),
                    depfile.Token('j/kl.o', depfile.TokenType.PATH),
                    depfile.Token('\n', depfile.TokenType.NEWLINE),
                ]
            ])


class TransformPathsTests(unittest.TestCase):

    def test_identity(self):
        dep_text = """p/r.s: f/g.h
a/b.o: ../e/d.c
"""
        self.assertEqual(
            depfile.transform_paths(dep_text, lambda s: s), dep_text)

    def test_transform(self):
        dep_text = """p/r.s: f/g.h
a/b.o: ../e/d.c
"""
        expected = """P/R.S: F/G.H
A/B.O: ../E/D.C
"""
        self.assertEqual(
            depfile.transform_paths(dep_text, lambda s: s.upper()), expected)


if __name__ == '__main__':
    unittest.main()
