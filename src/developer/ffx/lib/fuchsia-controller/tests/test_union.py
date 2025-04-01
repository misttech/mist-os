# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest

import fidl.fuchsia_controller_test as fc_test

from fidl import FrameworkError


class UnionTestSuite(unittest.TestCase):
    """Tests for FIDL union types"""

    def test_union_equal(self) -> None:
        self.assertEqual(
            fc_test.NoopUnion(union_str="foo"),
            fc_test.NoopUnion(union_str="foo"),
        )

    def test_union_not_equal(self) -> None:
        self.assertNotEqual(
            fc_test.NoopUnion(union_str="foo"),
            fc_test.NoopUnion(union_str="bar"),
        )

    def test_union_equal_default(self) -> None:
        self.assertEqual(
            fc_test.NoopUnion.make_default(), fc_test.NoopUnion.make_default()
        )

    def test_union_not_equal_default(self) -> None:
        self.assertNotEqual(
            fc_test.NoopUnion.make_default(), fc_test.NoopUnion(union_str="foo")
        )

    def test_flexible_union_empty(self) -> None:
        fc_test.EmptyUnion()

    def test_union_multiple_variants(self) -> None:
        with self.assertRaises(TypeError):
            fc_test.NoopUnion(union_str="foo", union_bool=True)

    def test_union_no_variants(self) -> None:
        with self.assertRaises(TypeError):
            fc_test.NoopUnion()

    def test_union_result_with_none_response(self) -> None:
        fc_test.FlexibleMethodTesterFlexibleTwoWayUnionResult(response=None)

    def test_union_make_default_then_fill(self) -> None:
        obj = fc_test.NoopUnion.make_default()
        obj._union_str = "foo"
        self.assertEqual(obj.union_str, "foo")

    def test_union_result_make_default_then_fill_response(self) -> None:
        obj = (
            fc_test.FlexibleMethodTesterFlexibleTwoWayUnionResult.make_default()
        )
        obj._response = None
        self.assertIsNone(obj.response)

    def test_union_result_make_default_then_fill_framework_err(self) -> None:
        obj = (
            fc_test.FlexibleMethodTesterFlexibleTwoWayUnionResult.make_default()
        )
        obj._framework_err = FrameworkError.UNKNOWN_METHOD
        self.assertEqual(
            obj,
            fc_test.FlexibleMethodTesterFlexibleTwoWayUnionResult(
                framework_err=FrameworkError.UNKNOWN_METHOD
            ),
        )
