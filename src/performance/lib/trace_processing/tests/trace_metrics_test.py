#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for trace metrics processors."""

import json
import os
import pathlib
import tempfile
from typing import Any, Dict, List
import unittest

import trace_processing.metrics.cpu as cpu_metrics
import trace_processing.metrics.fps as fps_metrics
import trace_processing.metrics.scenic as scenic_metrics
import trace_processing.trace_importing as trace_importing
import trace_processing.trace_metrics as trace_metrics
import trace_processing.trace_model as trace_model


class TestCaseResultTest(unittest.TestCase):
    """Tests TestCaseResult"""

    def test_to_json(self) -> None:
        label = "L1"
        test_suite = "bar"

        result = trace_metrics.TestCaseResult(
            label=label,
            unit=trace_metrics.Unit.bytesPerSecond,
            values=[0, 0.1, 23.45, 6],
        )

        self.assertEqual(result.label, label)
        self.assertEqual(
            result.to_json(test_suite=test_suite),
            {
                "label": label,
                "test_suite": test_suite,
                "unit": "bytes/second",
                "values": [0, 0.1, 23.45, 6],
            },
        )

    def test_write_fuchsia_perf_json(self) -> None:
        test_suite = "ts"
        results = [
            trace_metrics.TestCaseResult(
                label="l1",
                unit=trace_metrics.Unit.percent,
                values=[1],
            ),
            trace_metrics.TestCaseResult(
                label="l2",
                unit=trace_metrics.Unit.framesPerSecond,
                values=[2],
            ),
        ]

        with tempfile.TemporaryDirectory() as tmpdir:
            actual_output_path = (
                pathlib.Path(tmpdir) / "actual_output.fuchsiaperf.json"
            )

            trace_metrics.TestCaseResult.write_fuchsiaperf_json(
                results,
                test_suite=test_suite,
                output_path=actual_output_path,
            )

            actual_output = json.loads(actual_output_path.read_text())
            self.assertEqual(
                actual_output, [r.to_json(test_suite) for r in results]
            )


class MetricProcessorsTest(unittest.TestCase):
    """Tests for the various MetricProcessors."""

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
        self.assertEqual(aggregated_results[0].label, "CpuP5")
        self.assertAlmostEqual(aggregated_results[0].values[0], 21.15)
        self.assertEqual(aggregated_results[1].label, "CpuP25")
        self.assertAlmostEqual(aggregated_results[1].values[0], 25.75)
        self.assertEqual(aggregated_results[2].label, "CpuP50")
        self.assertAlmostEqual(aggregated_results[2].values[0], 31.5)
        self.assertEqual(aggregated_results[3].label, "CpuP75")
        self.assertAlmostEqual(aggregated_results[3].values[0], 37.25)
        self.assertEqual(aggregated_results[4].label, "CpuP95")
        self.assertAlmostEqual(aggregated_results[4].values[0], 41.85)
        self.assertEqual(aggregated_results[5].label, "CpuMin")
        self.assertAlmostEqual(aggregated_results[5].values[0], 20.0)
        self.assertEqual(aggregated_results[6].label, "CpuMax")
        self.assertAlmostEqual(aggregated_results[6].values[0], 43.0)
        self.assertEqual(aggregated_results[7].label, "CpuAverage")
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
        self.assertEqual(aggregated_results[0].label, "CpuP5")
        self.assertAlmostEqual(aggregated_results[0].values[0], 21.15)
        self.assertEqual(aggregated_results[1].label, "CpuP25")
        self.assertAlmostEqual(aggregated_results[1].values[0], 25.75)
        self.assertEqual(aggregated_results[2].label, "CpuP50")
        self.assertAlmostEqual(aggregated_results[2].values[0], 31.5)
        self.assertEqual(aggregated_results[3].label, "CpuP75")
        self.assertAlmostEqual(aggregated_results[3].values[0], 37.25)
        self.assertEqual(aggregated_results[4].label, "CpuP95")
        self.assertAlmostEqual(aggregated_results[4].values[0], 41.85)
        self.assertEqual(aggregated_results[5].label, "CpuMin")
        self.assertAlmostEqual(aggregated_results[5].values[0], 20.0)
        self.assertEqual(aggregated_results[6].label, "CpuMax")
        self.assertAlmostEqual(aggregated_results[6].values[0], 43.0)
        self.assertEqual(aggregated_results[7].label, "CpuAverage")
        self.assertAlmostEqual(aggregated_results[7].values[0], 31.5)

    def test_fps_metric(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            os.path.join(self._runtime_deps_path, "fps_metric.json")
        )
        results: List[
            trace_metrics.TestCaseResult
        ] = fps_metrics.metrics_processor(model, {})
        self.assertAlmostEqual(results[0].values[0], 10000000.0)
        self.assertAlmostEqual(results[0].values[1], 5000000.0)
        aggregated_results: List[
            trace_metrics.TestCaseResult
        ] = fps_metrics.metrics_processor(model, {"aggregateMetricsOnly": True})
        self.assertEqual(len(aggregated_results), 8)
        self.assertEqual(aggregated_results[0].label, "FpsP5")
        self.assertAlmostEqual(aggregated_results[0].values[0], 5250000.0)
        self.assertEqual(aggregated_results[1].label, "FpsP25")
        self.assertAlmostEqual(aggregated_results[1].values[0], 6250000.0)
        self.assertEqual(aggregated_results[2].label, "FpsP50")
        self.assertAlmostEqual(aggregated_results[2].values[0], 7500000.0)
        self.assertEqual(aggregated_results[3].label, "FpsP75")
        self.assertAlmostEqual(aggregated_results[3].values[0], 8750000.0)
        self.assertEqual(aggregated_results[4].label, "FpsP95")
        self.assertAlmostEqual(aggregated_results[4].values[0], 9750000.0)
        self.assertEqual(aggregated_results[5].label, "FpsMin")
        self.assertAlmostEqual(aggregated_results[5].values[0], 5000000.0)
        self.assertEqual(aggregated_results[6].label, "FpsMax")
        self.assertAlmostEqual(aggregated_results[6].values[0], 10000000.0)
        self.assertEqual(aggregated_results[7].label, "FpsAverage")
        self.assertAlmostEqual(aggregated_results[7].values[0], 7500000.0)

    def test_scenic_metrics(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            os.path.join(self._runtime_deps_path, "scenic_metric.json")
        )
        results: List[
            trace_metrics.TestCaseResult
        ] = scenic_metrics.metrics_processor(model, {})
        self.assertAlmostEqual(results[0].values[0], 0.09)
        self.assertAlmostEqual(results[0].values[1], 0.08)
        self.assertAlmostEqual(results[0].values[2], 0.1)
        self.assertAlmostEqual(results[1].values[0], 0.11)
        self.assertAlmostEqual(results[1].values[1], 0.112)
        aggregated_results: List[
            trace_metrics.TestCaseResult
        ] = scenic_metrics.metrics_processor(
            model, {"aggregateMetricsOnly": True}
        )
        self.assertEqual(len(aggregated_results), 16)
        self.assertEqual(aggregated_results[0].label, "RenderCpuP5")
        self.assertAlmostEqual(aggregated_results[0].values[0], 0.081)
        self.assertEqual(aggregated_results[1].label, "RenderCpuP25")
        self.assertAlmostEqual(aggregated_results[1].values[0], 0.085)
        self.assertEqual(aggregated_results[2].label, "RenderCpuP50")
        self.assertAlmostEqual(aggregated_results[2].values[0], 0.09)
        self.assertEqual(aggregated_results[3].label, "RenderCpuP75")
        self.assertAlmostEqual(aggregated_results[3].values[0], 0.095)
        self.assertEqual(aggregated_results[4].label, "RenderCpuP95")
        self.assertAlmostEqual(aggregated_results[4].values[0], 0.099)
        self.assertEqual(aggregated_results[5].label, "RenderCpuMin")
        self.assertAlmostEqual(aggregated_results[5].values[0], 0.08)
        self.assertEqual(aggregated_results[6].label, "RenderCpuMax")
        self.assertAlmostEqual(aggregated_results[6].values[0], 0.1)
        self.assertEqual(aggregated_results[7].label, "RenderCpuAverage")
        self.assertAlmostEqual(aggregated_results[7].values[0], 0.09)
        self.assertEqual(aggregated_results[8].label, "RenderTotalP5")
        self.assertAlmostEqual(aggregated_results[8].values[0], 0.1101)
        self.assertEqual(aggregated_results[9].label, "RenderTotalP25")
        self.assertAlmostEqual(aggregated_results[9].values[0], 0.1105)
        self.assertEqual(aggregated_results[10].label, "RenderTotalP50")
        self.assertAlmostEqual(aggregated_results[10].values[0], 0.111)
        self.assertEqual(aggregated_results[11].label, "RenderTotalP75")
        self.assertAlmostEqual(aggregated_results[11].values[0], 0.1115)
        self.assertEqual(aggregated_results[12].label, "RenderTotalP95")
        self.assertAlmostEqual(aggregated_results[12].values[0], 0.1119)
        self.assertEqual(aggregated_results[13].label, "RenderTotalMin")
        self.assertAlmostEqual(aggregated_results[13].values[0], 0.11)
        self.assertEqual(aggregated_results[14].label, "RenderTotalMax")
        self.assertAlmostEqual(aggregated_results[14].values[0], 0.112)
        self.assertEqual(aggregated_results[15].label, "RenderTotalAverage")
        self.assertAlmostEqual(aggregated_results[15].values[0], 0.111)
