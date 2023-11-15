# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Provides the implementation for simple performance tests which run a test
component that publishes a fuchsiaperf.json file.
"""

import logging
import pathlib

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from perf_publish import publish
from mobly import asserts, test_runner

_DEFAULT_FUCHSIAPERF_JSON = "results.fuchsiaperf.json"
_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaComponentPerfTest(fuchsia_base_test.FuchsiaBaseTest):
    """
    Mobly test class allowing to run a test component and publish its fucshiaperf data.

    Required Mobly Test Params:
        expected_metric_names_filename (str): Name of the file with the metric allowlist.
        ffx_test_url (str): Test URL to execute via `ffx test run`.

    Optional Mobly Test Params:
        ffx_test_options(list[str]): Test options to supply to `ffx test run`
    """

    def test_fuchsia_component(self) -> None:
        """Run a test component in the device and publish its perf data.

        This function launches a test component in the device, collects its
        fuchsiaperf data and then publishes it ensuring that the expected
        metrics are present.
        """
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self.ffx_test_options: list[str] = self.user_params["ffx_test_options"]
        self.ffx_test_url: str = self.user_params["ffx_test_url"]
        self.expected_metric_names_filename = self.user_params[
            "expected_metric_names_filename"
        ]

        cmd = [
            "test",
            "run",
            self.ffx_test_url,
            "--output-directory",
            self.test_case_path,
        ] + self.ffx_test_options
        _LOGGER.info("Running: %s", " ".join(cmd))
        self.device.ffx.run(cmd, timeout=None, capture_output=False)
        result_files = list(
            pathlib.Path(self.test_case_path).rglob(_DEFAULT_FUCHSIAPERF_JSON)
        )
        asserts.assert_equal(len(result_files), 1)
        publish.publish_fuchsiaperf(
            result_files,
            self.expected_metric_names_filename,
        )


if __name__ == "__main__":
    test_runner.main()
