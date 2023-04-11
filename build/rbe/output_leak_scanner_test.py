#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import contextlib
import io
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import output_leak_scanner

from typing import Any, Sequence


def _strs(items: Sequence[Any]) -> Sequence[str]:
    return [str(i) for i in items]


def _paths(items: Sequence[Any]) -> Sequence[Path]:
    return [Path(i) for i in items]


def _write_text_file_contents(path: Path, contents: str):
    with open(path, 'w') as f:
        f.write(contents)


def _write_binary_file_contents(path: Path, contents: bytearray):
    with open(path, 'wb') as f:
        f.write(contents)


class MainArgParserTests(unittest.TestCase):

    def test_empty(self):
        parser = output_leak_scanner._MAIN_ARG_PARSER
        args = parser.parse_args([])
        self.assertIsNone(args.label)
        self.assertTrue(args.execute)
        self.assertEqual(args.outputs, [])

    def test_label(self):
        label = '//foo:bar'
        parser = output_leak_scanner._MAIN_ARG_PARSER
        args = parser.parse_args(['--label', label])
        self.assertEqual(args.label, label)

    def test_execute(self):
        parser = output_leak_scanner._MAIN_ARG_PARSER
        args = parser.parse_args(['--execute'])
        self.assertTrue(args.execute)

    def test_no_execute(self):
        parser = output_leak_scanner._MAIN_ARG_PARSER
        args = parser.parse_args(['--no-execute'])
        self.assertFalse(args.execute)

    def test_outputs(self):
        outputs = ['foo/bar.txt', 'bar/foo.o']
        parser = output_leak_scanner._MAIN_ARG_PARSER
        args = parser.parse_args(outputs)  # positional
        self.assertEqual(args.outputs, outputs)


class ErrorMsgTests(unittest.TestCase):

    def test_no_label(self):
        s = io.StringIO()
        with contextlib.redirect_stdout(s):
            output_leak_scanner.error_msg('oops')
        self.assertIn('oops', s.getvalue())

    def test_label(self):
        s = io.StringIO()
        with contextlib.redirect_stdout(s):
            output_leak_scanner.error_msg('oops', label='LABEL')
        self.assertIn('LABEL', s.getvalue())
        self.assertIn('oops', s.getvalue())


class WholeWordPatternTests(unittest.TestCase):

    def test_add_boundaries(self):
        self.assertEqual(
            output_leak_scanner._whole_word_pattern('zyx'), r'\bzyx\b')

    def test_no_extra_boundaries(self):
        self.assertEqual(
            output_leak_scanner._whole_word_pattern(r'\bqwer\b'), r'\bqwer\b')


class FileContainsSubpathTests(unittest.TestCase):

    def test_ignore_nonexistent(self):
        with tempfile.TemporaryDirectory() as td:
            self.assertFalse(
                output_leak_scanner.file_contains_subpath(
                    Path(td) / 'nonexistent', '.'))

    def test_ignore_dir(self):
        with tempfile.TemporaryDirectory() as td:
            self.assertFalse(
                output_leak_scanner.file_contains_subpath(Path(td), '.'))

    def test_text_no_match(self):
        with tempfile.TemporaryDirectory() as td:
            tf = (Path(td) / __name__).with_suffix('.txt')
            _write_text_file_contents(tf, 'a\nb\nc\n')
            self.assertFalse(
                output_leak_scanner.file_contains_subpath(tf, 'def'))

    def test_text_match(self):
        with tempfile.TemporaryDirectory() as td:
            tf = (Path(td) / __name__).with_suffix('.txt')
            _write_text_file_contents(tf, 'a\nb\nc\n')
            self.assertTrue(output_leak_scanner.file_contains_subpath(tf, 'b'))

    def test_text_no_match_partial_word(self):
        with tempfile.TemporaryDirectory() as td:
            tf = (Path(td) / __name__).with_suffix('.txt')
            _write_text_file_contents(tf, 'ab\nbb\nbc\n')
            self.assertFalse(output_leak_scanner.file_contains_subpath(tf, 'b'))
            self.assertTrue(output_leak_scanner.file_contains_subpath(tf, 'bb'))

    def test_binary_no_match(self):
        with tempfile.TemporaryDirectory() as td:
            tf = (Path(td) / __name__).with_suffix('.txt')
            _write_binary_file_contents(tf, b'\xcc\n\xdd\xee\n\xff\n+abc+\n')
            self.assertFalse(
                output_leak_scanner.file_contains_subpath(tf, 'pdq'))

    def test_binary_match(self):
        with tempfile.TemporaryDirectory() as td:
            tf = (Path(td) / __name__).with_suffix('.txt')
            _write_binary_file_contents(tf, b'\xcc\n\xdd\xee\n\xff\n+abc+\n')
            self.assertTrue(
                output_leak_scanner.file_contains_subpath(tf, 'abc'))

    def test_binary_match_partial(self):
        with tempfile.TemporaryDirectory() as td:
            tf = (Path(td) / __name__).with_suffix('.txt')
            _write_binary_file_contents(tf, b'\xcc\n\xdd\xee\n\xff\n+abc+\n')
            self.assertTrue(output_leak_scanner.file_contains_subpath(tf, 'b'))


class PathsWithBuildDirLeaksTests(unittest.TestCase):

    def test_negatives(self):
        cases = _paths(
            [
                'build/me/there', 'out/default', 'rebuild/me/here',
                'build/me/heretic'
            ])
        pattern = re.compile(r'\bbuild/me/here\b')
        actual = list(
            output_leak_scanner.paths_with_build_dir_leaks(cases, pattern))
        self.assertEqual(actual, [])

    def test_positives(self):
        cases = _paths(
            [
                '/tmp/build/me/here', 'build/me/here',
                'build/me/here/foo/bar.txt'
            ])
        pattern = re.compile(r'\bbuild/me/here\b')
        actual = set(
            output_leak_scanner.paths_with_build_dir_leaks(cases, pattern))
        self.assertEqual(actual, set(cases))


class TokensWithBuildDirLeaks(unittest.TestCase):

    def test_negatives(self):
        cases = [
            '-f', 'build/not/here', 'out/default', 'rebuild/here',
            'build/heretic'
        ]
        pattern = re.compile(r'\bbuild/here\b')
        actual = list(
            output_leak_scanner.tokens_with_build_dir_leaks(cases, pattern))
        self.assertEqual(actual, [])

    def test_positives(self):
        cases = ['/work/out/build/here', 'build/here', 'build/here/out.txt']
        pattern = re.compile(r'\bbuild/here\b')
        actual = list(
            output_leak_scanner.tokens_with_build_dir_leaks(cases, pattern))
        self.assertEqual(actual, cases)


class PreflightChecksTests(unittest.TestCase):

    def test_no_findings(self):
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            return_code = output_leak_scanner.preflight_checks(
                paths=[], command=[], pattern=re.compile('any'))
            self.assertEqual(return_code, 0)
        self.assertEqual(stdout.getvalue(), '')  # quiet

    def test_path_leak(self):
        stdout = io.StringIO()
        bad = 'as/df/g.h'
        with contextlib.redirect_stdout(stdout):
            return_code = output_leak_scanner.preflight_checks(
                paths=[bad], command=[], pattern=re.compile(r'\bas/df\b'))
            self.assertEqual(return_code, 1)
        message = stdout.getvalue()
        self.assertIn("Error", message)
        self.assertIn(bad, message)

    def test_command_leak(self):
        stdout = io.StringIO()
        bad = 'zs/df/g.h'
        with contextlib.redirect_stdout(stdout):
            return_code = output_leak_scanner.preflight_checks(
                paths=[],
                command=['touch', bad],
                pattern=re.compile(r'\bzs/df\b'))
            self.assertEqual(return_code, 1)
        message = stdout.getvalue()
        self.assertIn("Error", message)
        self.assertIn(bad, message)


class PostflightChecksTests(unittest.TestCase):

    def test_no_findings(self):
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(output_leak_scanner, 'file_contains_subpath',
                                   return_value=False) as mock_search:
                return_code = output_leak_scanner.postflight_checks(
                    outputs=['foo.o'], subpath='out/f/b')
                self.assertEqual(return_code, 0)
        mock_search.assert_called_once()
        self.assertEqual(stdout.getvalue(), '')

    def test_has_findings(self):
        stdout = io.StringIO()
        build_dir = 'out/f/b'
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(output_leak_scanner, 'file_contains_subpath',
                                   return_value=True) as mock_search:
                return_code = output_leak_scanner.postflight_checks(
                    outputs=['foo.o'], subpath=build_dir)
                self.assertEqual(return_code, 1)
        mock_search.assert_called_once()
        message = stdout.getvalue()
        self.assertIn("Error", message)
        self.assertIn(build_dir, message)


class ScanLeaksTests(unittest.TestCase):

    def test_missing_ddash(self):
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            # --help will trigger call to sys.exit, which we
            # want to avoid in testing.
            with mock.patch.object(sys, 'exit') as mock_exit:
                return_code = output_leak_scanner.scan_leaks(
                    [],
                    exec_root='/home',
                    working_dir='/home/build',
                )
        mock_exit.assert_called_once()
        self.assertEqual(return_code, 1)
        message = stdout.getvalue()
        self.assertIn("Error", message)
        self.assertIn("'--' is missing", message)

    def test_no_execute(self):
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(output_leak_scanner, 'preflight_checks',
                                   return_value=0) as mock_pre_check:
                with mock.patch.object(subprocess, 'call',
                                       return_value=1) as mock_call:
                    return_code = output_leak_scanner.scan_leaks(
                        ['--no-execute', '--', 'touch', 'down.txt'],
                        exec_root='/home',
                        working_dir='/home/build',
                    )
            mock_pre_check.assert_called_once()
            mock_call.assert_not_called()
        self.assertEqual(return_code, 0)
        message = stdout.getvalue()
        self.assertEqual(message, '')

    def test_execute_command_error(self):
        stdout = io.StringIO()
        output = 'down.txt'
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(subprocess, 'call',
                                   return_value=2) as mock_call:
                return_code = output_leak_scanner.scan_leaks(
                    [output, '--', 'touch', output],
                    exec_root='/home',
                    working_dir='/home/build',
                )
            mock_call.assert_called_once()
        self.assertEqual(return_code, 2)
        message = stdout.getvalue()
        self.assertEqual(message, '')

    def test_execute_success_preflight_error(self):
        stdout = io.StringIO()
        output = 'down.txt'
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(output_leak_scanner, 'preflight_checks',
                                   return_value=1) as mock_pre_check:
                with mock.patch.object(output_leak_scanner, 'postflight_checks',
                                       return_value=0) as mock_post_check:
                    with mock.patch.object(subprocess, 'call',
                                           return_value=0) as mock_call:
                        return_code = output_leak_scanner.scan_leaks(
                            [output, '--', 'touch', output],
                            exec_root='/home',
                            working_dir='/home/build',
                        )
            mock_pre_check.assert_called_once()
            mock_call.assert_called_once()
            mock_post_check.assert_called_once()
        self.assertEqual(return_code, 1)

    def test_execute_success_postflight_error(self):
        stdout = io.StringIO()
        output = 'down.txt'
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(output_leak_scanner, 'preflight_checks',
                                   return_value=0) as mock_pre_check:
                with mock.patch.object(output_leak_scanner, 'postflight_checks',
                                       return_value=1) as mock_post_check:
                    with mock.patch.object(subprocess, 'call',
                                           return_value=0) as mock_call:
                        return_code = output_leak_scanner.scan_leaks(
                            [output, '--', 'touch', output],
                            exec_root='/home',
                            working_dir='/home/build',
                        )
            mock_pre_check.assert_called_once()
            mock_call.assert_called_once()
            mock_post_check.assert_called_once()
        self.assertEqual(return_code, 1)

    def test_execute_success_scans_clean(self):
        stdout = io.StringIO()
        output = 'down.txt'
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(output_leak_scanner, 'preflight_checks',
                                   return_value=0) as mock_pre_check:
                with mock.patch.object(output_leak_scanner, 'postflight_checks',
                                       return_value=0) as mock_post_check:
                    with mock.patch.object(subprocess, 'call',
                                           return_value=0) as mock_call:
                        return_code = output_leak_scanner.scan_leaks(
                            [output, '--', 'touch', output],
                            exec_root='/home',
                            working_dir='/home/build',
                        )
            mock_pre_check.assert_called_once()
            mock_call.assert_called_once()
            mock_post_check.assert_called_once()
        self.assertEqual(return_code, 0)


if __name__ == '__main__':
    unittest.main()
