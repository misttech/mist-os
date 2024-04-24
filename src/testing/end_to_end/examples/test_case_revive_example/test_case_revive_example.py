# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Example usage of test_case_revive.TestCaseRevive."""

import logging

from test_case_revive import test_case_revive
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class ExampleTestCaseRevive(test_case_revive.TestCaseRevive):
    """Example usage of test_case_revive.TestCaseRevive."""

    # TODO(https://fxbug.dev/332381215): mypy complains about this decorator not
    # being typed, but it is. Ignoring this for now, until we fix it.
    @test_case_revive.tag_test(tag_name="revive_test_case")
    def test_firmware_version(self) -> None:
        for fuchsia_device in self.fuchsia_devices:
            _LOGGER.info(
                "%s is running on %s firmware",
                fuchsia_device.device_name,
                fuchsia_device.firmware_version,
            )


if __name__ == "__main__":
    test_runner.main()
