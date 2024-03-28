#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Input Latency Benchmark."""

from fuchsia_base_test import fuchsia_base_test
from honeydew.typing import ui as ui_custom_types
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import test_runner

TOUCH_APP = (
    "fuchsia-pkg://fuchsia.com/flatland-examples#meta/"
    "simplest-app-flatland-session.cm"
)


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
            self.dut.user_input.tap(
                location=ui_custom_types.Coordinate(x=500, y=500),
                tap_event_count=100,
                duration=3350,
            )

        # TODO(b/271467734): Process fxt tracing file.


if __name__ == "__main__":
    test_runner.main()
