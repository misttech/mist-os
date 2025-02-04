#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Input Latency Benchmark."""

import os
from pathlib import Path

import test_data
from fuchsia_base_test import fuchsia_base_test
from honeydew.affordances.ui.user_input import types as ui_custom_types
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import test_runner
from perf_publish import publish
from trace_processing import trace_importing, trace_metrics, trace_model
from trace_processing.metrics import input_latency

TOUCH_APP = (
    "fuchsia-pkg://fuchsia.com/flatland-examples#meta/"
    "simplest-app-flatland-session.cm"
)
TEST_NAME: str = "fuchsia.input_latency.simplest_app"


class InputBenchmark(fuchsia_base_test.FuchsiaBaseTest):
    """Input Benchmarks.

    Attributes:
        dut: FuchsiaDevice object.

    This test traces touch input performance in
    ui/examples/simplest-app-flatland-session.
    """

    def setup_test(self) -> None:
        super().setup_test()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

        # Stop the session for a clean state.
        self.dut.session.stop()

        self.dut.session.start()

    def teardown_test(self) -> None:
        self.dut.session.stop()

    def test_logic(self) -> None:
        # Add simplest-input-flatland-session-app to session.
        self.dut.session.add_component(TOUCH_APP)

        touch_device = self.dut.user_input.create_touch_device()

        with self.dut.tracing.trace_session(
            categories=[
                "input",
                "gfx",
                "magma",
            ],
            buffer_size=36,
            download=True,
            directory=self.log_path,
            trace_file="trace.fxt",
        ):
            # Each tap will be 33.5ms apart, drifting 0.166ms against regular 60
            # fps vsync interval. 100 taps span the entire vsync interval 1 time at
            # 100 equidistant points.
            touch_device.tap(
                location=ui_custom_types.Coordinate(x=500, y=500),
                tap_event_count=100,
                duration_ms=3350,
            )

        expected_trace_filename: str = os.path.join(self.log_path, "trace.fxt")

        json_trace_file: str = trace_importing.convert_trace_file_to_json(
            expected_trace_filename
        )

        model: trace_model.Model = trace_importing.create_model_from_file_path(
            json_trace_file
        )

        input_latency_results = input_latency.metrics_processor(
            model, {"aggregateMetricsOnly": False}
        )

        fuchsiaperf_json_path = Path(
            os.path.join(self.log_path, f"{TEST_NAME}.fuchsiaperf.json")
        )

        trace_metrics.TestCaseResult.write_fuchsiaperf_json(
            results=input_latency_results,
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
