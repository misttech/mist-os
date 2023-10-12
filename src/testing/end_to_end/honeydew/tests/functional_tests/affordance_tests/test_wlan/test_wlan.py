#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for wlan affordance."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts
from mobly import test_runner

from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)


class WlanTests(fuchsia_base_test.FuchsiaBaseTest):
    """Wlan affordance tests"""

    def setup_class(self) -> None:
        """Setup function that will be called before executing any test in the
        class.

        To signal setup failure, use asserts or raise your own exception.

        Errors raised from `setup_class` will trigger `on_fail`.

        Implementation is optional.
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_get_phy_ids(self) -> None:
        """Test case for wlan.get_phy_ids().

        This test gets physical specifications for implementing WLAN on this device.
        """
        res = self.device.wlan.get_phy_ids()
        asserts.assert_is_not_none(res, [])


if __name__ == "__main__":
    test_runner.main()
