# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner


class CpuProfilerEndToEndTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_test(self) -> None:
        super().setup_test()
        self.device = self.fuchsia_devices[0]

    def test_launch_component(self) -> None:
        component_url = (
            "fuchsia-pkg://fuchsia.com/demo_target#meta/demo_target.cm"
        )
        output_json = json.loads(
            self.device.ffx.run(
                [
                    "--machine",
                    "json",
                    "profiler",
                    "launch",
                    "--url",
                    component_url,
                    "--duration",
                    "2",
                    "--print-stats",
                    "--symbolize",
                    "false",
                ],
                capture_output=True,
            )
        )
        asserts.assert_greater(output_json["samples_collected"], 10)


if __name__ == "__main__":
    test_runner.main()
