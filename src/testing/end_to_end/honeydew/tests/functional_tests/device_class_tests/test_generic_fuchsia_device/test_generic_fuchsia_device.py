#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for device_classes/generic_fuchsia_device.py."""

import logging

from honeydew.device_classes import generic_fuchsia_device
from mobly import asserts, test_runner
from test_fuchsia_device_base import test_fuchsia_device_base

_LOGGER = logging.getLogger(__name__)


# pylint: disable=too-few-public-methods
class GenericFuchsiaDeviceTests(test_fuchsia_device_base.FuchsiaDeviceBaseTests
                               ):
    """GenericFuchsia device tests"""

    def test_device_instance(self):
        """Test case to make sure DUT is a GenericFuchsiaDevice device"""
        asserts.assert_is_instance(
            self.device, generic_fuchsia_device.GenericFuchsiaDevice)


if __name__ == '__main__':
    test_runner.main()
