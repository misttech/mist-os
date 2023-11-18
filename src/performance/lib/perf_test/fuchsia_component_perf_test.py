# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Provides the implementation for simple performance tests which run a test
component that publishes a fuchsiaperf.json file.
"""

import logging
import os
import pathlib

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from perf_publish import publish
from mobly import asserts, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaComponentPerfTest(fuchsia_base_test.FuchsiaBaseTest):
    """
    Mobly test class allowing to run a test component and publish its fuchsiaperf data.

    Required Mobly Test Params:
        expected_metric_names_filepath (str): Name of the file with the metric allowlist.
        ffx_test_url (str): Test URL to execute via `ffx test run`.

    Optional Mobly Test Params:
        ffx_test_options(list[str]): Test options to supply to `ffx test run`
            Default: []
        test_component_args (list[str]): Options to supply to the test component.
            Default: []
        results_path_test_arg (str): The option to be used by the test for the results path.
            Important: this is just the option (ex: "--out"). The value will be passed by the test.
            Default: None
        process_runs (int): Number of times to run the test component.
            Default: 1
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
        self.expected_metric_names_filepath: str = self.user_params[
            "expected_metric_names_filepath"
        ]
        self.process_runs = self.user_params.get("process_runs", 1)
        self.results_path_test_arg = self.user_params.get(
            "results_path_test_arg"
        )
        self.test_component_args = self.user_params.get(
            "test_component_args", []
        )

        result_files: list[str] = []
        for i in range(self.process_runs):
            options: list[str] = self.ffx_test_options[:]
            options.append("--")
            options += self.test_component_args

            results_file = f"results_process{i}.fuchsiaperf_full.json"
            results_file_path = f"/custom_artifacts/{results_file}"
            if self.results_path_test_arg:
                if self.results_path_test_arg.endswith("="):
                    options.append(
                        f"{self.results_path_test_arg}{results_file_path}"
                    )
                else:
                    options += [
                        self.results_path_test_arg,
                        results_file_path,
                    ]
            else:
                options.append(results_file_path)

            test_dir = os.path.join(self.test_case_path, f"ffx_test_{i}")
            cmd = [
                "test",
                "run",
                self.ffx_test_url,
                "--output-directory",
                test_dir,
            ] + options
            _LOGGER.info("Running: ffx %s", " ".join(cmd))
            self.device.ffx.run(cmd, timeout=None, capture_output=False)

            test_result_files = list(pathlib.Path(test_dir).rglob(results_file))
            asserts.assert_equal(len(test_result_files), 1)

            dest_file = os.path.join(self.test_case_path, results_file)
            os.rename(test_result_files[0], dest_file)
            result_files.append(dest_file)

        publish.publish_fuchsiaperf(
            result_files,
            os.path.basename(self.expected_metric_names_filepath),
        )


if __name__ == "__main__":
    test_runner.main()
