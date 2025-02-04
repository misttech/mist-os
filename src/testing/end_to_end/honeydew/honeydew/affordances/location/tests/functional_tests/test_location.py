# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for location affordance."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

from honeydew.affordances.connectivity.wlan.utils.types import CountryCode
from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)

TIMEOUT_COUNTRY_CODE_SEC = 10.0
"""Seconds to wait for the country code to propagate to WLAN PHYs."""


class LocationTests(fuchsia_base_test.FuchsiaBaseTest):
    """Location affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_set_region(self) -> None:
        """Verify set_region() works on device."""
        self.device.location.set_region(CountryCode.UNITED_STATES_OF_AMERICA)

    def test_set_region_fails(self) -> None:
        """Verify set_region() fails on device with incorrect args."""

        # Do not expect an error. wlancfg's regulatory_manager does not
        # propagate an error up to RegulatoryRegionConfigurator when setting an
        # invalid country code.
        #
        # TODO(http://b/370600007): Replace with assert_raises once there is
        # error checking in set_region.
        self.device.location.set_region("??")


if __name__ == "__main__":
    test_runner.main()
