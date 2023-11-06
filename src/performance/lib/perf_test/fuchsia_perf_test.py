# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utilities to run test components and publish fuchsiaperf data."""

import pathlib

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.transports import ffx
from mobly import asserts
from perf_publish import publish

_DEFAULT_FUCHSIAPERF_JSON = "results.fuchsiaperf.json"
_DEFAULT_RESULTS_PATH = f"/custom_artifacts/{_DEFAULT_FUCHSIAPERF_JSON}"


def _url(package_name: str, component_name: str) -> str:
    return f"fuchsia-pkg://fuchsia.com/{package_name}#meta/{component_name}"


class FuchsiaPerfTest(fuchsia_base_test.FuchsiaBaseTest):
    """
    Mobly base test class providing utilities to run test components and publish fucshiaperf data.
    """

    def setup_test(self) -> None:
        super().setup_test()
        self.device = fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self.ffx: ffx.FFX = ffx.FFX(self.device.device_name)

    def run_test_component(
        self,
        package_name: str,
        component_name: str,
        expected_metrics_name_filename: str,
    ) -> None:
        """Run a test component in the device and publish its perf data.

        This function launches a test component in the device, collects its
        fuchsiaperf data and then publishes it ensuring that the expected
        metrics are present.
        """
        self.ffx.run(
            [
                "test",
                "run",
                _url(package_name, component_name),
                "--output-directory",
                self.test_case_path,
                "--",
                _DEFAULT_RESULTS_PATH,
            ],
            timeout=None,
            capture_output=False,
        )
        publish.publish_fuchsiaperf(
            list(
                pathlib.Path(self.test_case_path).rglob(
                    _DEFAULT_FUCHSIAPERF_JSON
                )
            ),
            expected_metrics_name_filename,
        )
