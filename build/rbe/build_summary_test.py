#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import sys
import unittest
from pathlib import Path
from unittest import mock
from typing import Iterable, Sequence

import build_summary


class LabelsToDictTests(unittest.TestCase):
    def test_one_label(self):
        self.assertEqual(
            build_summary.labels_to_dict("bar=foo"), {"bar": "foo"}
        )

    def test_two_labels(self):
        self.assertEqual(
            build_summary.labels_to_dict("baz=1,spice=sugar"),
            {"baz": "1", "spice": "sugar"},
        )

    def test_duplicate_last_wins(self):
        self.assertEqual(
            build_summary.labels_to_dict("bar=foo,bar=quux"), {"bar": "quux"}
        )


class GetActionCategoryFromLabelsTests(unittest.TestCase):
    def test_cxx(self):
        self.assertEqual(
            build_summary.get_action_category_from_labels(
                "lang=cpp,type=compile,tool=clang"
            ),
            "cxx",
        )

    def test_link(self):
        self.assertEqual(
            build_summary.get_action_category_from_labels(
                "type=link,tool=clang"
            ),
            "link",
        )

    def test_rust(self):
        self.assertEqual(
            build_summary.get_action_category_from_labels(
                "type=tool,toolname=rustc"
            ),
            "rust",
        )

    def test_custom_tool(self):
        self.assertEqual(
            build_summary.get_action_category_from_labels(
                "type=tool,toolname=protoc"
            ),
            "protoc",
        )

    def test_unknown(self):
        self.assertEqual(
            build_summary.get_action_category_from_labels("type=tool"), "other"
        )


class GetActionCategoryAndMetricTests(unittest.TestCase):
    def test_metric_for_all(self):
        self.assertEqual(
            build_summary.get_action_category_and_metric("Foo.Metadata.Time"),
            (None, "Foo.Metadata.Time"),
        )

    def test_metric_for_one_tool(self):
        self.assertEqual(
            build_summary.get_action_category_and_metric(
                "[toolname=catter].Foo.Metadata.Time"
            ),
            ("catter", "Foo.Metadata.Time"),
        )


if __name__ == "__main__":
    unittest.main()
