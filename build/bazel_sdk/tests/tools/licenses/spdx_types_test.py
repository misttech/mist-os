# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for spdx_types.py"""

import unittest
from fuchsia.tools.licenses.spdx_types import *


class TestSpdxTypes(unittest.TestCase):

    def test_license_expression_simple(self):
        input = "LicenseRef-Some-Id.1"
        exp = SpdxLicenseExpression.create(input)
        self.assertEqual((input,), exp.license_ids)
        self.assertEqual("{0}", exp.expression_template)
        self.assertEqual(input, exp.serialize())

    def test_license_expression_non_standard(self):
        input = "License-13a9b0a42ef7d1ff"
        exp = SpdxLicenseExpression.create(input)
        self.assertEqual((input,), exp.license_ids)
        self.assertEqual("{0}", exp.expression_template)
        self.assertEqual(input, exp.serialize())

    def test_license_expression_complex(self):
        input = "LicenseRef-X AND (LicenseRef-Y+ OR LicenseRef-Z WITH LicenseRef-X)"
        exp = SpdxLicenseExpression.create(input)
        self.assertEqual(
            ("LicenseRef-X", "LicenseRef-Y", "LicenseRef-Z"), exp.license_ids)
        self.assertEqual(
            "{0} AND ({1}+ OR {2} WITH {0})", exp.expression_template)
        self.assertEqual(input, exp.serialize())

    def test_license_expression_replace_ids(self):
        input = "LicenseRef-X AND (LicenseRef-Y+ OR LicenseRef-Z WITH LicenseRef-X)"
        exp = SpdxLicenseExpression.create(input)

        id_replacer = SpdxIdReplacer(SpdxIdFactory.new_license_id_factory())
        id_replacer.new_id("LicenseRef-X")
        id_replacer.new_id("LicenseRef-Y")
        id_replacer.new_id("LicenseRef-Z")
        exp = exp.replace_license_ids(id_replacer)

        self.assertEqual(
            "LicenseRef-0 AND (LicenseRef-1+ OR LicenseRef-2 WITH LicenseRef-0)",
            exp.serialize())


if __name__ == '__main__':
    unittest.main()
