#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import pathlib
import unittest
from pathlib import Path
from unittest import mock

import cl_utils


class FlattenCommaListTests(unittest.TestCase):

    def test_empty(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list([])),
            [],
        )

    def test_singleton(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list(['qwe'])),
            ['qwe'],
        )

    def test_one_comma(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list(['qw,er'])),
            ['qw', 'er'],
        )

    def test_two_items(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list(['as', 'df'])),
            ['as', 'df'],
        )

    def test_multiple_items_with_commas(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list(['as,12', 'df', 'zx,cv,bn'])),
            ['as', '12', 'df', 'zx', 'cv', 'bn'],
        )


class ExpandFusedFlagsTests(unittest.TestCase):

    def test_empty(self):
        self.assertEqual(
            list(cl_utils.expand_fused_flags([], ['-Z'])),
            [],
        )

    def test_no_expand(self):
        self.assertEqual(
            list(cl_utils.expand_fused_flags(['-Yfoo'], ['-Z'])),
            ['-Yfoo'],
        )

    def test_expand_one(self):
        self.assertEqual(
            list(cl_utils.expand_fused_flags(['-Yfoo'], ['-Y'])),
            ['-Y', 'foo'],
        )

    def test_expand_multiple(self):
        self.assertEqual(
            list(
                cl_utils.expand_fused_flags(
                    ['-Xxx', '-Yfog', '-Dbar'], {'-Y', '-X'})),
            ['-X', 'xx', '-Y', 'fog', '-Dbar'],
        )

    def test_already_expanded(self):
        self.assertEqual(
            list(cl_utils.expand_fused_flags(['-Y', 'foo'], ['-Y'])),
            ['-Y', 'foo'],
        )

    def test_expand_repeated(self):
        self.assertEqual(
            list(
                cl_utils.expand_fused_flags(
                    ['-Yfoo=f', 'other', '-Ybar=g'], ['-Y'])),
            ['-Y', 'foo=f', 'other', '-Y', 'bar=g'],
        )


class FuseExpandedFlagsTests(unittest.TestCase):

    def test_empty(self):
        self.assertEqual(
            list(cl_utils.fuse_expanded_flags([], {'-Z'})),
            [],
        )

    def test_no_fuse(self):
        self.assertEqual(
            list(cl_utils.fuse_expanded_flags(['-Y', 'foo'], {'-Z'})),
            ['-Y', 'foo'],
        )

    def test_fuse_one(self):
        self.assertEqual(
            list(cl_utils.fuse_expanded_flags(['-Y', 'foo'], {'-Y'})),
            ['-Yfoo'],
        )

    def test_already_fused(self):
        self.assertEqual(
            list(cl_utils.fuse_expanded_flags(['-Wfoo'], {'-W'})),
            ['-Wfoo'],
        )

    def test_fuse_repeated(self):
        self.assertEqual(
            list(
                cl_utils.fuse_expanded_flags(
                    ['-W', 'zoo', 'blah', '-W', 'woof'], {'-W'})),
            ['-Wzoo', 'blah', '-Wwoof'],
        )


class KeyedFlagsToValuesDictTests(unittest.TestCase):

    def test_empty(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict([]),
            dict(),
        )

    def test_key_no_value(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(['a', 'z']),
            {
                'a': [],
                'z': [],
            },
        )

    def test_blank_string_values(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(['b=', 'b=', 'e=']),
            {
                'b': ['', ''],
                'e': [''],
            },
        )

    def test_no_repeat_keys(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(['a=b', 'c=d']),
            {
                'a': ['b'],
                'c': ['d'],
            },
        )

    def test_repeat_keys(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(['a=b', 'c=d', 'a=b', 'c=e']),
            {
                'a': ['b', 'b'],
                'c': ['d', 'e'],
            },
        )

    def test_convert_values_to_int(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(
                ['a=7', 'c=8'], convert_type=int),
            {
                'a': [7],
                'c': [8],
            },
        )

    def test_convert_values_to_path(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(
                ['a=/foo/bar', 'c=bar/foo.quux'], convert_type=Path),
            {
                'a': [Path('/foo/bar')],
                'c': [Path('bar/foo.quux')],
            },
        )


class LastValueOrDefaultTests(unittest.TestCase):

    def test_default(self):
        self.assertEqual(
            cl_utils.last_value_or_default([], '3'),
            '3',
        )

    def test_last_value(self):
        self.assertEqual(
            cl_utils.last_value_or_default(['1', '2', '5', '6'], '4'),
            '6',
        )


class LastValueOfDictFlagTests(unittest.TestCase):

    def test_default_no_key(self):
        self.assertEqual(
            cl_utils.last_value_of_dict_flag(
                {
                    'f': ['g', 'h'],
                    'p': []
                }, 'z', 'default'),
            'default',
        )

    def test_default_empty_values(self):
        self.assertEqual(
            cl_utils.last_value_of_dict_flag(
                {
                    'f': ['g', 'h'],
                    'p': []
                }, 'p', 'boring'),
            'boring',
        )

    def test_last_value(self):
        self.assertEqual(
            cl_utils.last_value_of_dict_flag(
                {
                    'f': ['g', 'h'],
                    'p': []
                }, 'f', 'boring'),
            'h',
        )


class SubprocessCallTests(unittest.TestCase):

    def test_success(self):
        result = cl_utils.subprocess_call(['echo', 'hello'])
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, ['hello'])
        self.assertEqual(result.stderr, [])
        self.assertGreater(result.pid, 0)

    def test_success_quiet(self):
        result = cl_utils.subprocess_call(['echo', 'hello'], quiet=True)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, ['hello'])  # still captured
        self.assertEqual(result.stderr, [])
        self.assertGreater(result.pid, 0)

    def test_failure(self):
        result = cl_utils.subprocess_call(['false'])
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, [])
        self.assertEqual(result.stderr, [])
        self.assertGreater(result.pid, 0)

    def test_error(self):
        result = cl_utils.subprocess_call(['ls', '/does/not/exist'])
        # error code is 2 on linux, 1 on darwin
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, [])
        self.assertIn('No such file or directory', result.stderr[0])
        self.assertIn('/does/not/exist', result.stderr[0])
        self.assertGreater(result.pid, 0)


if __name__ == '__main__':
    unittest.main()
