# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import logging

from mobly import test_runner

from fuchsia_power_base_test_lib import fuchsia_power_base_test
from perf_publish import publish
from power_test_utils import power_test_utils

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
        super().test_launch_hermetic_test()

        metrics_processor = power_test_utils.PowerMetricsProcessor(
            power_samples_path=self.power_trace_path
        )
        metrics_processor.process_metrics()
        fuchsiaperf_json_path = metrics_processor.write_fuchsiaperf_json(
            output_dir=self.log_path,
            metric_name=self.metric_name,
            trace_results=[],
        )
        publish.publish_fuchsiaperf(
            [fuchsiaperf_json_path],
            f"fuchsia.power.{self.metric_name}.txt",
        )


if __name__ == "__main__":
    test_runner.main()
