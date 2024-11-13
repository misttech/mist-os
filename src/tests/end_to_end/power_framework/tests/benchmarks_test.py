# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import logging
import os

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner
from trace_processing import trace_importing, trace_model

_LOGGER = logging.getLogger(__name__)


class PowerBenchmarksTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_class(self) -> None:
        """Initialize all DUT(s)"""
        super().setup_class()
        self.device = self.fuchsia_devices[0]

    def test_integration_testcase(self) -> None:
        _LOGGER.info("Running power framework benchmarks Lacewing test...")
        # The power-framework-bench-integration runs the benchmark for
        # Takewakelease
        # Toggle levels
        host_output_path = self.test_case_path
        with self.device.tracing.trace_session(
            categories=[
                "kernel:sched",
                "kernel:meta",
                "power-broker",
            ],
            buffer_size=36,
            download=True,
            directory=host_output_path,
            trace_file="trace.fxt",
        ):
            self.device.ffx.run_test_component(
                "fuchsia-pkg://fuchsia.com/power-framework-bench-integration-tests#meta/integration.cm",
                ffx_test_args=[
                    "--realm",
                    "/core/testing/system-tests",
                    "--test-filter",
                    "*test_topologytestdaemon_toggle",
                ],
                test_component_args=[
                    "--repeat",
                    "400",
                ],
                capture_output=False,
            )

        json_trace_file: str = trace_importing.convert_trace_file_to_json(
            os.path.join(host_output_path, "trace.fxt")
        )
        _LOGGER.info("Json Trace file name: %s", json_trace_file)
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            json_trace_file
        )

        # TODO: b/357447550 - we need something similar for trace processing
        #     and upload
        """
        trace_results = power.PowerMetricsProcessor().process_metrics(model)

        trace_metrics.TestCaseResult.write_fuchsiaperf_json(
            results=trace_results,
            test_suite="Manual",
            output_path=pathlib.Path(args.output_path),
        )

        fuchsiaperf_data = [
            {
                "test_suite": "fuchsia.power.framework",
                "label": "ToggleLease",
                "values": [1.015],
                "unit": "ms",
            },
            {
                "test_suite": "fuchsia.power.framework",
                "label": "TakeWakeLease",
                "values": [0.750],
                "unit": "ms",
            },
        ]
        test_perf_file = os.path.join(self.log_path, "test.fuchsiaperf.json")
        with open(test_perf_file, "w") as f:
            json.dump(fuchsiaperf_data, f, indent=4)

        publish.publish_fuchsiaperf([test_perf_file], ...)
        """


if __name__ == "__main__":
    test_runner.main()
