# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import asserts, test_runner
from trace_processing import trace_importing, trace_model, trace_utils


class PerfTestTraceEventsTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_test(self) -> None:
        super().setup_test()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_perftest_library_trace_events(self) -> None:
        with self.device.tracing.trace_session(
            categories=[
                "kernel",
                "perftest",
            ],
            buffer_size=36,
            download=True,
            directory=self.log_path,
            trace_file="trace.fxt",
        ):
            self.device.ffx.run_test_component(
                "fuchsia-pkg://fuchsia.com/fuchsia_microbenchmarks#meta/fuchsia_microbenchmarks.cm",
                ffx_test_args=["--realm", "/core/testing/system-tests"],
                test_component_args=[
                    "-p",
                    "--quiet",
                    "--runs",
                    "4",
                    "--enable-tracing",
                    "--filter=^Null$",
                ],
                capture_output=False,
            )

        json_trace_file: str = trace_importing.convert_trace_file_to_json(
            os.path.join(self.log_path, "trace.fxt")
        )
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            json_trace_file
        )
        event_names = [
            event.name
            for event in trace_utils.filter_events(
                model.all_events(),
                category="perftest",
                type=trace_model.Event,
            )
        ]
        asserts.assert_equal(
            event_names,
            [
                "test_group",
                "test_setup",
                "test_run",
                "test_run",
                "test_run",
                "test_run",
                "test_teardown",
            ],
        )


if __name__ == "__main__":
    test_runner.main()
