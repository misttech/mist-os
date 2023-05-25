#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for generic_fuchsia_device.py device class."""

import logging

from honeydew import transports
from honeydew.device_classes.fuchsia_controller import \
    generic_fuchsia_device as fc_generic_fuchsia_device
from honeydew.device_classes.sl4f import \
    generic_fuchsia_device as sl4f_generic_fuchsia_device
from mobly import asserts
from mobly import test_runner
from test_fuchsia_device import test_fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)


class GenericFuchsiaDeviceTests(test_fuchsia_device.FuchsiaDeviceTests):
    """GenericFuchsia device tests"""

    def test_device_instance(self) -> None:
        """Test case to make sure DUT is a GenericFuchsiaDevice device"""

        if self._is_fuchsia_controller_based_device(self.device):
            asserts.assert_is_instance(
                self.device, fc_generic_fuchsia_device.GenericFuchsiaDevice)
        else:
            asserts.assert_is_instance(
                self.device, sl4f_generic_fuchsia_device.GenericFuchsiaDevice)


if __name__ == '__main__':
    test_runner.main()
