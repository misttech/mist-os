# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
import unittest.mock as mock

import args


class TestArgs(unittest.TestCase):
    def test_empty(self):
        flags = args.parse_args([])
        flags.validate()

    @mock.patch("args.termout.is_valid", return_value=False)
    def test_validation_errors(self, _mock: mock.Mock):
        for arg_list in [
            ["--simple", "--status"],
            ["--simple", "--style"],
            ["--status"],
        ]:
            flags = args.parse_args(arg_list)
            try:
                with self.assertRaises(args.FlagError):
                    flags.validate()
            except AssertionError:
                raise AssertionError("Expected FlagError from " + str(arg_list))

    def test_simple(self):
        flags = args.parse_args(["--simple"])
        flags.validate()
        self.assertEqual(flags.style, False)
        self.assertEqual(flags.status, False)
