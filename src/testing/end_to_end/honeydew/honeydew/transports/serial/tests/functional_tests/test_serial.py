#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Serial transport."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

from honeydew.fuchsia_device import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)


class SerialTransportTests(fuchsia_base_test.FuchsiaBaseTest):
    """Serial transport tests"""

    def setup_class(self) -> None:
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_send(self) -> None:
        """Test case for Serial.send()"""
        for cmd in [
            "echo hi",
            "echo hello",
            "echo foo",
            "echo bar",
            "ls",
            "ls -l",
        ]:
            self.device.serial.send(
                cmd=cmd,
            )


if __name__ == "__main__":
    test_runner.main()
