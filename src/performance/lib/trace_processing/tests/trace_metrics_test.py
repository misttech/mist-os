#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for trace metrics processors."""

import os
from typing import Any, Dict, List
import unittest

import trace_processing.metrics.cpu as cpu_metrics
import trace_processing.trace_importing as trace_importing
import trace_processing.trace_metrics as trace_metrics
import trace_processing.trace_model as trace_model


class TraceMetricsTest(unittest.TestCase):
    """Trace metrics tests."""

    def setUp(self):
        # A second dirname is required to account for the .pyz archive which
        # contains the test and a third one since data is a sibling of the test.
        self._runtime_deps_path: str = os.path.join(
            os.path.dirname(os.path.dirname(os.path.dirname(__file__))),
            "runtime_deps",
        )

    def test_custom_processor(self) -> None:
        def test_processor(
            model: trace_model.Model, extra_args: Dict[str, Any]
        ) -> List[trace_metrics.TestCaseResult]:
            return [
                trace_metrics.TestCaseResult(
                    "test", trace_metrics.Unit.countBiggerIsBetter, [1234, 5678]
                )
            ]

        model: trace_model.Model = trace_model.Model()
        metrics_spec: trace_metrics.MetricsSpec = trace_metrics.MetricsSpec(
            name="test",
            processor=test_processor,
        )
        results: List[
            trace_metrics.TestCaseResult
        ] = metrics_spec.process_metrics(model)
        self.assertAlmostEqual(results[0].values[0], 1234.0)
        self.assertAlmostEqual(results[0].values[1], 5678.0)

    def test_cpu_metric(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            os.path.join(self._runtime_deps_path, "cpu_metric.json")
        )
        results: List[
            trace_metrics.TestCaseResult
        ] = cpu_metrics.metrics_processor(model, {})
        self.assertAlmostEqual(results[0].values[0], 43.0)
        self.assertAlmostEqual(results[0].values[1], 20.0)
        aggregated_results: List[
            trace_metrics.TestCaseResult
        ] = cpu_metrics.metrics_processor(model, {"aggregateMetricsOnly": True})
        self.assertEqual(len(aggregated_results), 8)
        self.assertEqual(aggregated_results[0].label, "cpu_p5")
        self.assertAlmostEqual(aggregated_results[0].values[0], 21.15)
        self.assertEqual(aggregated_results[1].label, "cpu_p25")
        self.assertAlmostEqual(aggregated_results[1].values[0], 25.75)
        self.assertEqual(aggregated_results[2].label, "cpu_p50")
        self.assertAlmostEqual(aggregated_results[2].values[0], 31.5)
        self.assertEqual(aggregated_results[3].label, "cpu_p75")
        self.assertAlmostEqual(aggregated_results[3].values[0], 37.25)
        self.assertEqual(aggregated_results[4].label, "cpu_p95")
        self.assertAlmostEqual(aggregated_results[4].values[0], 41.85)
        self.assertEqual(aggregated_results[5].label, "cpu_min")
        self.assertAlmostEqual(aggregated_results[5].values[0], 20.0)
        self.assertEqual(aggregated_results[6].label, "cpu_max")
        self.assertAlmostEqual(aggregated_results[6].values[0], 43.0)
        self.assertEqual(aggregated_results[7].label, "cpu_average")
        self.assertAlmostEqual(aggregated_results[7].values[0], 31.5)

    def test_cpu_metric_after_system_metrics_logger_migration(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            os.path.join(
                self._runtime_deps_path, "cpu_metric_system_metrics_logger.json"
            )
        )
        results: List[
            trace_metrics.TestCaseResult
        ] = cpu_metrics.metrics_processor(model, {})
        self.assertAlmostEqual(results[0].values[0], 43.0)
        self.assertAlmostEqual(results[0].values[1], 20.0)
        aggregated_results: List[
            trace_metrics.TestCaseResult
        ] = cpu_metrics.metrics_processor(model, {"aggregateMetricsOnly": True})
        self.assertEqual(len(aggregated_results), 8)
        self.assertEqual(aggregated_results[0].label, "cpu_p5")
        self.assertAlmostEqual(aggregated_results[0].values[0], 21.15)
        self.assertEqual(aggregated_results[1].label, "cpu_p25")
        self.assertAlmostEqual(aggregated_results[1].values[0], 25.75)
        self.assertEqual(aggregated_results[2].label, "cpu_p50")
        self.assertAlmostEqual(aggregated_results[2].values[0], 31.5)
        self.assertEqual(aggregated_results[3].label, "cpu_p75")
        self.assertAlmostEqual(aggregated_results[3].values[0], 37.25)
        self.assertEqual(aggregated_results[4].label, "cpu_p95")
        self.assertAlmostEqual(aggregated_results[4].values[0], 41.85)
        self.assertEqual(aggregated_results[5].label, "cpu_min")
        self.assertAlmostEqual(aggregated_results[5].values[0], 20.0)
        self.assertEqual(aggregated_results[6].label, "cpu_max")
        self.assertAlmostEqual(aggregated_results[6].values[0], 43.0)
        self.assertEqual(aggregated_results[7].label, "cpu_average")
        self.assertAlmostEqual(aggregated_results[7].values[0], 31.5)
