#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for the metrics allowlist code."""

import os
import tempfile
import unittest

import perf_publish.metrics_allowlist as metrics_allowlist


class CatapultConverterTest(unittest.TestCase):
    """Catapult converter metric publishing tests"""

    def test_expected_metrics(self) -> None:
        """Test case that asserts the expected test metrics and ignoring comments"""
        with tempfile.TemporaryDirectory() as tmpdir:
            metrics_file = os.path.join(tmpdir, "metrics.txt")
            with open(metrics_file, "w") as f:
                f.write(
                    "# Comment line\n\n"
                    "fuchsia.suite1: foo\n"
                    "fuchsia.suite1: bar\n"
                )
            allowlist = metrics_allowlist.MetricsAllowlist(metrics_file)
        self.assertEquals(
            allowlist.expected_metrics,
            set(["fuchsia.suite1: foo", "fuchsia.suite1: bar"]),
        )
        self.assertEquals(allowlist.optional_metrics, set())

        # This succeeds without raising any exception.
        allowlist.check(set(["fuchsia.suite1: foo", "fuchsia.suite1: bar"]))

        with self.assertRaises(ValueError) as context:
            allowlist.check(set(["fuchsia.suite1: foo", "fuchsia.suite1: new"]))

        self.assertIn(
            (
                "-fuchsia.suite1: bar\n"
                " fuchsia.suite1: foo\n"
                "+fuchsia.suite1: new\n"
            ),
            str(context.exception),
        )

    def test_optional_metrics(self) -> None:
        """Test case that asserts optional metrics behavior"""
        with tempfile.TemporaryDirectory() as tmpdir:
            metrics_file = os.path.join(tmpdir, "metrics.txt")
            with open(metrics_file, "w") as f:
                f.write(
                    "fuchsia.suite1: foo\n"
                    "fuchsia.suite1: bar\n"
                    "fuchsia.suite1: opt1 [optional]\n"
                    "fuchsia.suite1: opt2 [optional]\n"
                )
            allowlist = metrics_allowlist.MetricsAllowlist(metrics_file)

        self.assertEquals(
            allowlist.expected_metrics,
            set(["fuchsia.suite1: foo", "fuchsia.suite1: bar"]),
        )
        self.assertEquals(
            allowlist.optional_metrics,
            set(["fuchsia.suite1: opt1", "fuchsia.suite1: opt2"]),
        )

        # These succeed without raising an exception.
        allowlist.check(set(["fuchsia.suite1: foo", "fuchsia.suite1: bar"]))
        allowlist.check(
            set(
                [
                    "fuchsia.suite1: foo",
                    "fuchsia.suite1: bar",
                    "fuchsia.suite1: opt1",
                ]
            )
        )

        with self.assertRaises(ValueError) as context:
            allowlist.check(
                set(
                    [
                        "fuchsia.suite1: foo",
                        "fuchsia.suite1: new",
                        "fuchsia.suite1: opt2",
                    ]
                )
            )

        self.assertIn(
            (
                "-fuchsia.suite1: bar\n"
                " fuchsia.suite1: foo\n"
                "+fuchsia.suite1: new\n"
                " fuchsia.suite1: opt1 [optional]\n"
                " fuchsia.suite1: opt2 [optional]"
            ),
            str(context.exception),
        )
