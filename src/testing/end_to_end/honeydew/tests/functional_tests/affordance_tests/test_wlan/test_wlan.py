#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for wlan affordance."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing.wlan import WlanMacRole

_LOGGER: logging.Logger = logging.getLogger(__name__)


# TODO(b/323406186): Add remaining mult-device functional tests
class WlanTests(fuchsia_base_test.FuchsiaBaseTest):
    """Wlan affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_get_phy_id_list(self) -> None:
        """Test case for wlan.get_phy_id_list().

        This test gets physical specifications for implementing WLAN on this device.
        """
        res = self.device.wlan.get_phy_id_list()
        asserts.assert_is_not_none(res, [])

    def test_iface_methods(self) -> None:
        """Test case for basic single device wlan iface methods.

        This test gets the list of phy IDs present, creates an iface, then checks that
        is exists by querying it, then calls destroy on the iface, and finally gets the
        iface ID list to check that the created iface has been successfully destroyed.

        This test case calls the following wlan methods:
            * wlan.get_phy_id_list()
            * wlan.create_iface()
            * wlan.query_iface()
            * wlan.destroy_iface()
            * wlan.get_iface_id_list()
        """
        # We check here to make sure there is an intel WiFi driver on the device. If
        # there is not one, we pass because there is no create_iface for a null
        # "sta_addr" field except in the intel WiFi drivers.
        # TODO(b/328500376): Add WLAN affordance method for this or remove if not
        # needed.
        driver_list = self.device.ffx.run(["driver", "list"])
        if driver_list.find("iwlwifi") != -1:
            phy_ids = self.device.wlan.get_phy_id_list()
            iface_ids = self.device.wlan.get_iface_id_list()

            iface_id = self.device.wlan.create_iface(
                phy_id=phy_ids[0], role=WlanMacRole.CLIENT
            )

            query_resp = self.device.wlan.query_iface(iface_id)
            asserts.assert_equal(query_resp.role, WlanMacRole.CLIENT)
            asserts.assert_equal(query_resp.id, iface_id)
            asserts.assert_equal(query_resp.phy_id, phy_ids[0])

            self.device.wlan.destroy_iface(iface_id)
            expected_iface_ids = self.device.wlan.get_iface_id_list()

            asserts.assert_equal(iface_ids, expected_iface_ids)
        else:
            _LOGGER.info(
                "This test is not applicable to the hardware setup. It is "
                "only used to test a functionality present on intel WiFi "
                "drivers."
            )


if __name__ == "__main__":
    test_runner.main()
