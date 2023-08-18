#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Flatland Benchmark."""

import os
import time

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import test_runner
from mobly import asserts

TILE_URL = "fuchsia-pkg://fuchsia.com/flatland-examples#meta/" \
           "flatland-view-provider.cm"

BENCHMARK_DURATION_SEC = 10


class FlatlandBenchmark(fuchsia_base_test.FuchsiaBaseTest):
    """Flatland Benchmark.

    Attributes:
        dut: FuchsiaDevice object.

    This test traces graphic performance in tile-session
    (src/ui/bin/tiles-session) and flatland-view-provider-example
    (src/ui/examples/flatland-view-provider).
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
        # Add flatland-view-provider tile
        self.dut.session.add_component(TILE_URL)

        # Initialize tracing session.
        self.dut.tracing.initialize(
            categories=[
                "input",
                "gfx",
                "magma",
                "system_metrics",
                "system_metrics_logger",
            ],
            buffer_size=36)

        # Start tracing.
        self.dut.tracing.start()

        time.sleep(BENCHMARK_DURATION_SEC)

        # Stop tracing.
        self.dut.tracing.stop()

        # Terminate the tracing session.
        trace_filename = self.dut.tracing.terminate_and_download(
            directory=self.log_path, trace_file="trace.fxt")

        expected_trace_filename = os.path.join(self.log_path, "trace.fxt")

        asserts.assert_equal(
            trace_filename, expected_trace_filename, msg="trace not downloaded")
        asserts.assert_true(
            os.path.exists(expected_trace_filename), msg="trace failed")

        # TODO(b/271467734): Process fxt tracing file.


if __name__ == "__main__":
    test_runner.main()
