# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.wlan.py."""

import unittest
from unittest import mock

from honeydew.affordances.fuchsia_controller.wlan import wlan
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport
from honeydew.typing.wlan import (
    BssDescription,
    BssType,
    ChannelBandwidth,
    InformationElementType,
    WlanChannel,
    WlanMacRole,
)

_TEST_SSID = "ThepromisedLAN"
_TEST_SSID_BYTES = list(str.encode(_TEST_SSID))

_TEST_BSS_DESC_1 = BssDescription(
    bssid=[1, 2, 3],
    bss_type=BssType.PERSONAL,
    beacon_period=2,
    capability_info=3,
    ies=[InformationElementType.SSID, len(_TEST_SSID)] + _TEST_SSID_BYTES,
    channel=WlanChannel(primary=1, cbw=ChannelBandwidth.CBW20, secondary80=3),
    rssi_dbm=4,
    snr_db=5,
)


# pylint: disable=protected-access
class WlanSL4FTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.sl4f.wlan.wlan.py."""

    def setUp(self) -> None:
        super().setUp()

        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice,
            autospec=True,
        )
        self.fc_transport_obj = mock.MagicMock(
            spec=fc_transport.FuchsiaController,
            autospec=True,
        )
        self.ffx_transport_obj = mock.MagicMock(
            spec=ffx_transport.FFX,
            autospec=True,
        )

        self.ffx_transport_obj.run.return_value = "".join(
            wlan._REQUIRED_CAPABILITIES
        )

        self.wlan_obj = wlan.Wlan(
            device_name="fuchsia-emulator",
            ffx=self.ffx_transport_obj,
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )

    def test_verify_supported(self) -> None:
        """Test if _verify_supported works."""
        self.ffx_transport_obj.run.return_value = ""
        with self.assertRaises(NotSupportedError):
            self.wlan_obj = wlan.Wlan(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
            )

    def test_connect(self) -> None:
        """Test if connect works."""
        # TODO(http://b/324949922): Finish implementation of Wlan.connect
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.connect("ssid", "password", _TEST_BSS_DESC_1)

    def test_create_iface(self) -> None:
        """Test if create_iface creates WLAN interfaces successfully."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.create_iface(1, WlanMacRole.CLIENT, None)

    def test_destroy_iface(self) -> None:
        """Test if destroy_iface works."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.destroy_iface(1)

    def test_disconnect(self) -> None:
        """Test if disconnect works."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.disconnect()

    def test_get_iface_id_list(self) -> None:
        """Test if get_iface_id_list works."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.get_iface_id_list()

    def test_get_country(self) -> None:
        """Test if get_country works."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.get_country(1)

    def test_get_phy_id_list(self) -> None:
        """Test if get_phy_id_list works."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.get_phy_id_list()

    def test_query_iface(self) -> None:
        """Test if query_iface works."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.query_iface(1)

    def test_scan_for_bss_info(self) -> None:
        """Test if scan_for_bss_info works."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.scan_for_bss_info()

    def test_set_region(self) -> None:
        """Test if set_region works."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.set_region("US")

    def test_status(self) -> None:
        """Test if status works."""
        # TODO(http://b/324138169): Finish implementation
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.status()


if __name__ == "__main__":
    unittest.main()
