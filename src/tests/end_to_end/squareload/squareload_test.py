# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import logging
import os

from mobly import test_runner

from fuchsia_power_base_test_lib import fuchsia_power_base_test
from perf_publish import publish
from power_test_utils import power_test_utils
from trace_processing import trace_importing, trace_metrics, trace_model
from trace_processing.metrics import cpu

_LOGGER: logging.Logger = logging.getLogger(__name__)


class SquareloadTest(fuchsia_power_base_test.FuchsiaPowerBaseTest):
    """Power performance test with square shaped CPU workload on target.

    Required Mobly Test Params:
        See fuchsia_power_base_test.py.
    """

    def test_launch_hermetic_test(self) -> None:
        """Executes a target-side workload while collecting power measurements.

        Compute and publish power metrics.
        """
        # Initialize host-side tracing and execute target workload.
        with self.device.tracing.trace_session(
            categories=[
                "kernel:sched",
                "kernel:meta",
                "starnix:atrace",
                "system_metrics_logger",
                "system_metrics",
                "memory_monitor",
            ],
            buffer_size=36,
            download=True,
            directory=self.log_path,
            trace_file=f"{self.metric_name}.fxt",
        ):
            super().test_launch_hermetic_test()

        # Process trace-based CPU metrics.
        trace_json_path: str = trace_importing.convert_trace_file_to_json(
            trace_path=os.path.join(self.log_path, f"{self.metric_name}.fxt")
        )

        model: trace_model.Model = trace_importing.create_model_from_file_path(
            trace_json_path
        )

        power_test_utils.merge_power_data(
            model,
            self.power_trace_path,
            os.path.join(self.log_path, f"{self.metric_name}.fxt"),
        )

        cpu_results: list[trace_metrics.TestCaseResult] = cpu.metrics_processor(
            model, {"aggregateMetricsOnly": True}
        )

        # Process non-trace-based power metrics.
        metrics_processor = power_test_utils.PowerMetricsProcessor(
            power_samples_path=self.power_trace_path
        )
        metrics_processor.process_metrics()
        fuchsiaperf_json_path = metrics_processor.write_fuchsiaperf_json(
            output_dir=self.log_path,
            metric_name=self.metric_name,
            trace_results=cpu_results,
        )
        publish.publish_fuchsiaperf(
            [fuchsiaperf_json_path],
            f"fuchsia.power.{self.metric_name}.txt",
        )


if __name__ == "__main__":
    test_runner.main()
