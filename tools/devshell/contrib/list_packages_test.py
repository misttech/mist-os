#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import contextlib
import io
import unittest

import list_packages


class TestPrintPackages(unittest.TestCase):

    def validate(self, input, output):
        f = io.StringIO()
        with contextlib.redirect_stdout(f):
            list_packages.print_packages(input)

        self.assertEqual(f.getvalue(), output)

    def test_single_set_single_package(self):
        self.validate({"set": ["package"]}, "package\n")

    def test_multiple_set_multiple_package(self):
        self.validate(
            {
                "set0": ["package0", "package2"],
                "set1": ["package1", "package3"]
            }, """package0
package1
package2
package3
""")


class TestPrintPackagesVerbose(unittest.TestCase):

    def validate(self, input, output):
        f = io.StringIO()
        with contextlib.redirect_stdout(f):
            list_packages.print_packages_verbose(input)

        self.assertEqual(f.getvalue(), output)

    def test_single_set_single_package(self):
        self.validate({"set": ["package"]}, "package [set]\n")

    def test_multiple_set_multiple_package(self):
        self.validate(
            {
                "set0": ["package0", "package2", "package4"],
                "set1": ["package1", "package3", "package4"]
            }, """package0 [set0]
package1 [set1]
package2 [set0]
package3 [set1]
package4 [set0 set1]
""")


class TestExtractPackagesFromListing(unittest.TestCase):

    def validate(self, listing, filter_, output):
        self.assertEqual(
            list_packages.extract_packages_from_listing(listing, filter_),
            output)

    def test_segment_in_manifests(self):
        self.validate(
            {"content": {
                "manifests": ["package"]
            }}, lambda s: True, ["package"])

    def test_path_in_manifests(self):
        self.validate(
            {"content": {
                "manifests": ["prefix/package"]
            }}, lambda s: True, ["package"])

    def test_filter(self):
        self.validate(
            {"content": {
                "manifests": ["package0", "package1"]
            }}, lambda s: s == "package1", ["package1"])


if __name__ == '__main__':
    unittest.main()
