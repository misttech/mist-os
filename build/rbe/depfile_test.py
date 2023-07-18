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

    def test_escape_slash(self):
        with self.assertRaises(ValueError):  # not handled yet
            self._test_tokens(
                [depfile.Token('\\\\', depfile.TokenType.ESCAPED)])

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


class ConsumeLineContinuationsTests(unittest.TestCase):

    def test_empty(self):
        toks = list(depfile.consume_line_continuations([]))
        self.assertEqual(toks, [])

    def test_continuations(self):
        toks = list(
            depfile.consume_line_continuations(
                [
                    depfile.Token('target/thing', depfile.TokenType.PATH),
                    depfile.Token(':', depfile.TokenType.COLON),
                    depfile.Token(' ', depfile.TokenType.SPACE),
                    depfile.Token('\\', depfile.TokenType.LINECONTINUE),
                    depfile.Token('\n', depfile.TokenType.NEWLINE),
                    depfile.Token(' ', depfile.TokenType.SPACE),
                    depfile.Token('src/dep1.cc', depfile.TokenType.PATH),
                    depfile.Token(' ', depfile.TokenType.SPACE),
                    depfile.Token('src/dep2.cc', depfile.TokenType.PATH),
                    depfile.Token('\n', depfile.TokenType.NEWLINE),
                ]))
        self.assertEqual(
            toks, [
                depfile.Token('target/thing', depfile.TokenType.PATH),
                depfile.Token(':', depfile.TokenType.COLON),
                depfile.Token(' ', depfile.TokenType.SPACE),
                depfile.Token(' ', depfile.TokenType.SPACE),
                depfile.Token('src/dep1.cc', depfile.TokenType.PATH),
                depfile.Token(' ', depfile.TokenType.SPACE),
                depfile.Token('src/dep2.cc', depfile.TokenType.PATH),
                depfile.Token('\n', depfile.TokenType.NEWLINE),
            ])


class ParseOneDepTests(unittest.TestCase):

    def test_empty(self):
        dep = depfile._parse_one_dep([])
        self.assertIsNone(dep)

    def test_blank_lines(self):
        dep = depfile._parse_one_dep(
            [
                depfile.Token('\n', depfile.TokenType.NEWLINE),
                depfile.Token('\n', depfile.TokenType.NEWLINE),
            ])
        self.assertIsNone(dep)

    def test_phony(self):
        dep = depfile._parse_one_dep(
            iter(
                [
                    # spaces have been filtered out
                    depfile.Token('target/thing', depfile.TokenType.PATH),
                    depfile.Token(':', depfile.TokenType.COLON),
                    depfile.Token('\n', depfile.TokenType.NEWLINE),
                ]))
        self.assertEqual(dep.target_paths, [Path('target/thing')])
        self.assertEqual(dep.deps_paths, [])

    def test_dep(self):
        dep = depfile._parse_one_dep(
            iter(
                [
                    # spaces have been filtered out
                    depfile.Token('target/thing', depfile.TokenType.PATH),
                    depfile.Token(':', depfile.TokenType.COLON),
                    depfile.Token('src/dep1.cc', depfile.TokenType.PATH),
                    depfile.Token('src/dep2.cc', depfile.TokenType.PATH),
                    depfile.Token('\n', depfile.TokenType.NEWLINE),
                ]))
        self.assertEqual(dep.target_paths, [Path('target/thing')])
        self.assertEqual(
            dep.deps_paths,
            [Path('src/dep1.cc'), Path('src/dep2.cc')])

    def test_dep_with_multiple_targets(self):
        dep = depfile._parse_one_dep(
            iter(
                [
                    # multiple targets
                    depfile.Token('target/thing', depfile.TokenType.PATH),
                    depfile.Token('target/thing.d', depfile.TokenType.PATH),
                    depfile.Token(':', depfile.TokenType.COLON),
                    depfile.Token('src/dep.cc', depfile.TokenType.PATH),
                    depfile.Token('\n', depfile.TokenType.NEWLINE),
                ]))
        self.assertEqual(
            dep.target_paths,
            [Path('target/thing'), Path('target/thing.d')])
        self.assertEqual(dep.deps_paths, [Path('src/dep.cc')])

    def test_missing_colon(self):
        with self.assertRaises(depfile.ParseError):
            depfile._parse_one_dep(
                iter(
                    [
                        depfile.Token(
                            'target/incomplete', depfile.TokenType.PATH),
                    ]))

    def test_unexpected_colon(self):
        with self.assertRaises(depfile.ParseError):
            depfile._parse_one_dep(
                iter([
                    depfile.Token(':', depfile.TokenType.COLON),
                ]))


class ParseLinesTestes(unittest.TestCase):

    def test_empty(self):
        deps = list(depfile.parse_lines([]))
        self.assertEqual(deps, [])

    def test_blank_lines(self):
        deps = list(depfile.parse_lines(['\n', '\r\n', '\n']))
        self.assertEqual(deps, [])

    def test_comments(self):
        deps = list(
            depfile.parse_lines(
                [
                    '# auto-generated, do not edit\n', '\r\n', '#comment\r\n',
                    '\n'
                ]))
        self.assertEqual(deps, [])

    def test_one_dep(self):
        deps = list(depfile.parse_lines(['a.out: bb.o\n']))
        self.assertEqual(len(deps), 1)
        self.assertEqual(deps[0].target_paths, [Path('a.out')])
        self.assertEqual(deps[0].deps_paths, [Path('bb.o')])
        self.assertFalse(deps[0].is_phony)

    def test_one_dep_line_continued(self):
        deps = list(depfile.parse_lines(['a.out: bb.o \\\n', ' cc.o dd.i\n']))
        self.assertEqual(len(deps), 1)
        self.assertEqual(deps[0].target_paths, [Path('a.out')])
        self.assertEqual(
            deps[0].deps_paths,
            [Path('bb.o'), Path('cc.o'),
             Path('dd.i')])
        self.assertFalse(deps[0].is_phony)

    def test_one_dep_line_continued_with_cr(self):
        deps = list(
            depfile.parse_lines(['a.out: \\\r\n', ' \\\r\n', ' cc.o dd.i\n']))
        self.assertEqual(len(deps), 1)
        self.assertEqual(deps[0].target_paths, [Path('a.out')])
        self.assertEqual(deps[0].deps_paths, [Path('cc.o'), Path('dd.i')])
        self.assertFalse(deps[0].is_phony)

    def test_multiple_deps(self):
        deps = list(
            depfile.parse_lines(
                ['a.out: bb.o\n', 'z.out :\\\n', 'y.h\n', 'p.out : \n']))
        self.assertEqual(len(deps), 3)

        self.assertEqual(deps[0].target_paths, [Path('a.out')])
        self.assertEqual(deps[0].deps_paths, [Path('bb.o')])
        self.assertFalse(deps[0].is_phony)

        self.assertEqual(deps[1].target_paths, [Path('z.out')])
        self.assertEqual(deps[1].deps_paths, [Path('y.h')])
        self.assertFalse(deps[1].is_phony)

        self.assertEqual(deps[2].target_paths, [Path('p.out')])
        self.assertEqual(deps[2].deps_paths, [])
        self.assertTrue(deps[2].is_phony)


if __name__ == '__main__':
    unittest.main()
