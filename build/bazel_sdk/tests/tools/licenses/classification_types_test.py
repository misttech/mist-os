# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for classification_types.py"""

import unittest
from fuchsia.tools.licenses.classification_types import *


class TestClassificationTypes(unittest.TestCase):

    def test_StringMatcher_to_json(self):
        sm = StringMatcher.create(["foo", "bar"])
        self.assertEqual(sm.to_json(), ["foo", "bar"])

    def test_StringMatcher_exact_matches(self):
        sm = StringMatcher.create(["foo", "bar"])

        self.assertTrue(sm.matches("foo"))
        self.assertTrue(sm.matches("bar"))
        self.assertFalse(sm.matches("baz"))

        self.assertEqual(sm.get_matches(["foo", "bar", "baz"]), ["foo", "bar"])
        self.assertEqual(
            sm.get_matches(["foo", "foo", "foo"]), ["foo", "foo", "foo"])

        self.assertTrue(sm.matches_all(["foo"]))
        self.assertTrue(sm.matches_all(["foo", "bar"]))
        self.assertTrue(sm.matches_all(["foo", "bar", "foo", "bar"]))
        self.assertFalse(sm.matches_all(["foo", "bar", "baz"]))
        self.assertFalse(sm.matches_all(["baz"]))

    def test_AsterixStringExpression_asterix_matches_everything(self):
        exp = AsterixStringExpression.create("*")

        self.assertTrue(exp.matches(""))
        self.assertTrue(exp.matches("foo"))

    def test_AsterixStringExpression_asterix_prefix(self):
        exp = AsterixStringExpression.create("*foo")

        self.assertTrue(exp.matches("foo"))
        self.assertTrue(exp.matches("  foo"))
        self.assertFalse(exp.matches("foo  "))

    def test_AsterixStringExpression_asterix_suffix(self):
        exp = AsterixStringExpression.create("foo*")

        self.assertTrue(exp.matches("foo"))
        self.assertTrue(exp.matches("foo  "))
        self.assertFalse(exp.matches("  foo"))

    def test_AsterixStringExpression_asterix_middle(self):
        exp = AsterixStringExpression.create("fo*o")

        self.assertTrue(exp.matches("foo"))
        self.assertTrue(exp.matches("fo   o"))
        self.assertFalse(exp.matches("fo   o  "))
        self.assertFalse(exp.matches(" fo   o"))

    def test_AsterixStringExpression_mulitple_asterix(self):
        exp = AsterixStringExpression.create("b*a*r")

        self.assertTrue(exp.matches("bar"))
        self.assertTrue(exp.matches("b a r"))
        self.assertTrue(exp.matches("ba r"))
        self.assertTrue(exp.matches("b ar"))
        self.assertFalse(exp.matches(" bar"))
        self.assertFalse(exp.matches("bar "))

        exp = AsterixStringExpression.create("*b*a*r")

        self.assertTrue(exp.matches(" bar"))
        self.assertTrue(exp.matches(" b a r"))
        self.assertFalse(exp.matches(" b a r "))

        exp = AsterixStringExpression.create("b*a*r*")

        self.assertTrue(exp.matches("bar "))
        self.assertTrue(exp.matches("b a r "))
        self.assertFalse(exp.matches(" b a r "))

        exp = AsterixStringExpression.create("*b*a*r*")

        self.assertTrue(exp.matches("b a r"))
        self.assertTrue(exp.matches(" b a r"))
        self.assertTrue(exp.matches("b a r "))
        self.assertTrue(exp.matches(" b a r "))

    def test_StringMatcher_asterix_matches(self):
        sm = StringMatcher.create(["fo*o", "*bar", "baz*", "*w*a*z*"])

        self.assertTrue(sm.matches("foo"))
        self.assertTrue(sm.matches("bar"))
        self.assertTrue(sm.matches("baz"))
        self.assertTrue(sm.matches("waz"))

        self.assertFalse(sm.matches("XYZ"))

        self.assertTrue(sm.matches("foXYZo"))
        self.assertFalse(sm.matches("XYZfoo"))
        self.assertFalse(sm.matches("fooXYZ"))
        self.assertFalse(sm.matches("fXYZoo"))

        self.assertTrue(sm.matches("XYZbar"))
        self.assertFalse(sm.matches("bXYZar"))
        self.assertFalse(sm.matches("barXYZ"))

        self.assertTrue(sm.matches("bazXYZ"))
        self.assertFalse(sm.matches("bXYZaz"))
        self.assertFalse(sm.matches("XYZbaz"))

        self.assertTrue(sm.matches("waz"))
        self.assertTrue(sm.matches("w a z"))
        self.assertTrue(sm.matches(" w a z "))
        self.assertFalse(sm.matches("z a w"))
        self.assertTrue(sm.matches("XwYaZz"))
        self.assertTrue(sm.matches("wXaYzZ"))


if __name__ == '__main__':
    unittest.main()
