# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Starnix affordance."""

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew import errors
from honeydew.fuchsia_device import fuchsia_device


class StarnixAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Starnix affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_run_console_shell_cmd(self) -> None:
        """Test case for Starnix.run_console_shell_cmd()"""
        if self.user_params["is_starnix_supported"]:
            self.device.starnix.run_console_shell_cmd(["echo", "hello"])
        else:
            with asserts.assert_raises(errors.NotSupportedError):
                self.device.starnix.run_console_shell_cmd(["echo", "hello"])


if __name__ == "__main__":
    test_runner.main()
