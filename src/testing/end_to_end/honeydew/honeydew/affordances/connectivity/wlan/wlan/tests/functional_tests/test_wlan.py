# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for wlan affordance."""

import logging
import time

from antlion.controllers import access_point
from antlion.controllers.ap_lib import hostapd_constants
from mobly import asserts, signals, test_runner
from wlan_base_test import wlan_base_test

from honeydew.affordances.connectivity.netstack.types import PortClass
from honeydew.affordances.connectivity.wlan.utils.types import (
    Authentication,
    ClientStatusConnected,
    ClientStatusConnecting,
    ClientStatusIdle,
    CountryCode,
    SecurityProtocol,
    WlanMacRole,
)
from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)

# Time to wait for a WLAN interface to become available.
WLAN_INTERFACE_TIMEOUT = 60

# Time to wait for country code change to take effect.
TIME_TO_WAIT_FOR_COUNTRY_CODE = 10


class WlanTests(wlan_base_test.WlanBaseTest):
    """Wlan affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
            * Assigns `access_point` variable with AccessPoint object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

        access_points: list[
            access_point.AccessPoint
        ] | None = self.register_controller(
            access_point, required=False, min_number=0
        )

        self.access_point: access_point.AccessPoint | None = (
            access_points[0] if access_points else None
        )

        self.wait_for_interface(self.device.netstack, PortClass.WLAN_CLIENT)

    def test_iface_methods(self) -> None:
        """Test case for device wlan iface methods.

        This test gets the list of phy IDs present, creates an iface, then checks that
        is exists by querying it, then calls destroy on the iface, and finally gets the
        iface ID list to check that the created iface has been successfully destroyed.

        This test case calls the following wlan methods:
            * wlan.get_phy_id_list()
            * wlan.get_iface_id_list()
            * wlan.create_iface()
            * wlan.query_iface()
            * wlan.destroy_iface()
        """
        # We check here to make sure the device is running a softmac WLAN driver.
        # If not, we run basic tests without create_iface().
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
            phy_ids = self.device.wlan.get_phy_id_list()
            iface_ids = self.device.wlan.get_iface_id_list()
            if iface_ids:
                self.device.wlan.destroy_iface(iface_ids[0])

    def test_basic_device_methods(self) -> None:
        """Test case for basic single device wlan methods.

        This test sets the region then gets the phy_ids present. It then checks that the
        country code set to the phy_id matches what was set at the start.

        This test case calls the following wlan methods:
            * wlan.set_region
            * wlan.get_phy_id_list
            * wlan.get_country
        """
        # TODO(http://b/337930095): Add the remaining board specific country code tests.
        phy_ids = self.device.wlan.get_phy_id_list()
        self.device.wlan.set_region(CountryCode.UNITED_STATES_OF_AMERICA)

        end_time = time.time() + TIME_TO_WAIT_FOR_COUNTRY_CODE
        while time.time() < end_time:
            country_resp = self.device.wlan.get_country(phy_ids[0])
            if country_resp == CountryCode.UNITED_STATES_OF_AMERICA:
                break
            _LOGGER.debug(f"Country code resp: {country_resp}")
            time.sleep(1)
        else:
            raise signals.TestFailure(
                f"Failed to set country code: {country_resp}"
            )

    def test_scan_and_connect(self) -> None:
        """Test case for scanning, connecting and disconnecting to a network.

        This test sets up an access point with a network, then scans and connects to
        that network. If the connect returns true we know it was successful. The test
        then calls disconnect and checks that the status is idle.

        This test case calls the following wlan methods:
            * wlan.scan_for_bss_info()
            * wlan.connect()
            * wlan.disconnect()
            * wlan.status()
        """
        if not self.access_point:
            raise signals.TestSkip("Access point required for this test")

        test_ssid = "test"
        access_point.setup_ap(
            access_point=self.access_point,
            profile_name="whirlwind",
            channel=hostapd_constants.AP_DEFAULT_CHANNEL_2G,
            ssid=test_ssid,
        )

        end_time = time.time() + 30
        while time.time() < end_time:
            iface_ids = self.device.wlan.get_iface_id_list()
            if len(iface_ids) > 0:
                break
        else:
            asserts.fail("No iface ids present")

        bss_scan_response = self.device.wlan.scan_for_bss_info()
        bss_desc_for_ssid = bss_scan_response.get(test_ssid)
        if bss_desc_for_ssid:
            asserts.assert_true(
                self.device.wlan.connect(
                    ssid=test_ssid,
                    password=None,
                    bss_desc=bss_desc_for_ssid[0],
                    authentication=Authentication(SecurityProtocol.OPEN, None),
                ),
                "Failed to connect.",
            )
        else:
            asserts.fail("Scan did not find bss descriptions for test ssid")

        self.device.wlan.disconnect()
        status = self.device.wlan.status()
        match status:
            case ClientStatusIdle():
                _LOGGER.debug(status)
            case ClientStatusConnecting():
                asserts.fail("Status did not return idle")
            case ClientStatusConnected():
                asserts.fail("Status did not return idle")
            case _:
                asserts.fail(
                    f"Did not return a valid status response: {status}"
                )


if __name__ == "__main__":
    test_runner.main()
