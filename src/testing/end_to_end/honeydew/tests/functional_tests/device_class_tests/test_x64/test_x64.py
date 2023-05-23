#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for x64.py device class."""

from honeydew.device_classes.sl4f import x64
from mobly import asserts
from mobly import test_runner
from test_fuchsia_device import test_fuchsia_device


class X64Tests(test_fuchsia_device.FuchsiaDeviceTests):
    """X64 device tests"""

    def test_device_instance(self) -> None:
        """Test case to make sure DUT is a X64 device"""
        asserts.assert_is_instance(self.device, x64.X64)


if __name__ == '__main__':
    test_runner.main()
