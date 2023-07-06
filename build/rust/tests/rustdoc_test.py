# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
from pathlib import Path
import sys

ROOT_PATH = Path(__file__) / "../.."
sys.path.append(str(ROOT_PATH / "tools/devshell"))
from rustdoc import parse_deps


class TestRustdoc(unittest.TestCase):
    def test_space(self):
        depinfo = "foo: bar/baz quux bl\\ ah\n"
        expected = {Path(p) for p in ("bar/baz", "quux", "bl ah")}
        self.assertSetEqual(expected, parse_deps(depinfo))

    def test_merging(self):
        depinfo = """
            one: a b c
            two: b c d f
        """
        expected = {Path(p) for p in "abcdf"}
        self.assertSetEqual(expected, parse_deps(depinfo))

    def test_empty(self):
        depinfo = """
            one:
            two: foo bar
            three:
        """
        expected = {Path(p) for p in ("foo", "bar")}
        self.assertSetEqual(expected, parse_deps(depinfo))

if __name__ == "__main__":
    unittest.main()
