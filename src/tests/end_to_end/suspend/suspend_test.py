# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import logging

from mobly import asserts, test_runner

from fuchsia_power_base_test_lib import fuchsia_power_base_test
from perf_publish import publish
from power_test_utils import power_test_utils
from trace_processing import trace_importing, trace_metrics, trace_model
from trace_processing.metrics import cpu


class SuspendTest(fuchsia_power_base_test.FuchsiaPowerBaseTest):
    """Suspend power performance test

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
                "kernel:syscall",
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
        trace_json_path: os.PathLike = (
            trace_importing.convert_trace_file_to_json(
                os.path.join(self.log_path, f"{self.metric_name}.fxt")
            )
        )

        model: trace_model.Model = trace_importing.create_model_from_file_path(
            trace_json_path
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
