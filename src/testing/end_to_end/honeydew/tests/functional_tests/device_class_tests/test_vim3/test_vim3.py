#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for vim3.py device class."""

from mobly import asserts
from mobly import test_runner
from test_fuchsia_device import test_fuchsia_device

from honeydew.device_classes.fuchsia_controller import vim3 as fc_vim3
from honeydew.device_classes.sl4f import vim3 as sl4f_vim3


class Vim3Tests(test_fuchsia_device.FuchsiaDeviceTests):
    """Vim3 device tests"""

    def test_device_instance(self) -> None:
        """Test case to make sure DUT is a Vim3 device"""
        if self._is_fuchsia_controller_based_device(self.device):
            asserts.assert_is_instance(self.device, fc_vim3.VIM3)
        else:
            asserts.assert_is_instance(self.device, sl4f_vim3.VIM3)


if __name__ == '__main__':
    test_runner.main()
