#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Flatland Benchmark."""

import itertools
import os
import time
from pathlib import Path

import test_data
from fuchsia_base_test import fuchsia_base_test
from honeydew.fuchsia_device import fuchsia_device
from mobly import asserts, test_runner
from perf_publish import publish
from trace_processing import trace_importing, trace_metrics, trace_model
from trace_processing.metrics import app_render, cpu

TILE_URL = (
    "fuchsia-pkg://fuchsia.com/flatland-examples#meta/flatland-rainbow.cm"
)
BENCHMARK_DURATION_SEC = 10
TEST_NAME: str = "fuchsia.app_render_latency"


class FlatlandBenchmark(fuchsia_base_test.FuchsiaBaseTest):
    """Flatland Benchmark.

    Attributes:
        dut: FuchsiaDevice object.

    This test traces graphic performance in tile-session
    (src/ui/bin/tiles-session) and flatland-rainbow-example
    (src/ui/examples/flatland-rainbow).
    """

    def setup_test(self) -> None:
        super().setup_test()

        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

        # Stop the session for a clean state.
        self.dut.session.stop()

        self.dut.session.start()

    def teardown_test(self) -> None:
        self.dut.session.stop()

    def test_flatland(self) -> None:
        # Add flatland-rainbow tile
        self.dut.session.add_component(TILE_URL)

        with self.dut.tracing.trace_session(
            categories=[
                "input",
                "gfx",
                "kernel:sched",
                "magma",
                "system_metrics",
                "system_metrics_logger",
            ],
            buffer_size=36,
            download=True,
            directory=self.log_path,
            trace_file="trace.fxt",
        ):
            time.sleep(BENCHMARK_DURATION_SEC)

        expected_trace_filename: str = os.path.join(self.log_path, "trace.fxt")

        asserts.assert_true(
            os.path.exists(expected_trace_filename), msg="trace failed"
        )

        json_trace_file: str = trace_importing.convert_trace_file_to_json(
            expected_trace_filename
        )

        model: trace_model.Model = trace_importing.create_model_from_file_path(
            json_trace_file
        )

        app_render_latency_results = (
            app_render.AppRenderLatencyMetricsProcessor(
                debug_name="flatland-rainbow-example",
                aggregates_only=True,
            ).process_metrics(model)
        )

        cpu_results = cpu.CpuMetricsProcessor(
            aggregates_only=False
        ).process_metrics(model)

        fuchsiaperf_json_path = Path(
            os.path.join(self.log_path, f"{TEST_NAME}.fuchsiaperf.json")
        )

        trace_metrics.TestCaseResult.write_fuchsiaperf_json(
            results=itertools.chain.from_iterable(
                (app_render_latency_results, cpu_results)
            ),
            test_suite=f"{TEST_NAME}",
            output_path=fuchsiaperf_json_path,
        )

        publish.publish_fuchsiaperf(
            fuchsia_perf_file_paths=[fuchsiaperf_json_path],
            expected_metric_names_filename=f"{TEST_NAME}.txt",
            test_data_module=test_data,
        )


if __name__ == "__main__":
    test_runner.main()
