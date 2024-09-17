# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Battery of tests of the lifecycle of ifaces managed by wlanix.
"""

import asyncio
import logging

import fidl.fuchsia_wlan_wlanix as fidl_wlanix
from antlion.controllers import fuchsia_device
from fuchsia_controller_py import Channel
from honeydew.typing.custom_types import FidlEndpoint
from mobly import base_test, test_runner
from mobly.asserts import abort_class_if, assert_equal, assert_is_not


class IfaceLifecycleTest(base_test.BaseTestClass):
    log: logging.Logger
    wlanix_proxy: fidl_wlanix.Wlanix.Client

    def setup_class(self) -> None:
        self.log = logging.getLogger()
        fuchsia_devices = self.register_controller(fuchsia_device)

        abort_class_if(
            len(fuchsia_devices) != 1, "Requires exactly one Fuchsia device"
        )
        abort_class_if(
            fuchsia_devices[0].honeydew_fd is None,
            "Requires a Honeydew-enabled FuchsiaDevice",
        )
        self.wlanix_proxy = fidl_wlanix.Wlanix.Client(
            fuchsia_devices[
                0
            ].honeydew_fd.fuchsia_controller.connect_device_proxy(
                FidlEndpoint("core/wlanix", "fuchsia.wlan.wlanix.Wlanix")
            )
        )

        proxy, server = Channel.create()
        self.wlanix_proxy.get_wifi(wifi=server.take())
        wifi_proxy = fidl_wlanix.Wifi.Client(proxy)

        response = asyncio.run(wifi_proxy.get_chip_ids()).unwrap()
        assert_is_not(
            response.chip_ids,
            None,
            "Wifi.GetChipIds() response is missing a chip_ids value",
        )
        assert_equal(
            len(response.chip_ids),
            1,
            "Wifi.GetChipIds() should return exactly one chip_id.",
        )

        self.chip_id = response.chip_ids[0]
        proxy, server = Channel.create()
        asyncio.run(
            wifi_proxy.get_chip(chip_id=self.chip_id, chip=server.take())
        ).unwrap()
        self.wifi_chip_proxy = fidl_wlanix.WifiChip.Client(proxy)

    def teardown_test(self) -> None:
        response = asyncio.run(
            self.wifi_chip_proxy.get_sta_iface_names()
        ).unwrap()
        assert_equal(
            len(response.iface_names),
            0,
            "Every test should end with no ifaces.",
        )

    def test_create_and_destroy_iface(self) -> None:
        response = asyncio.run(
            self.wifi_chip_proxy.get_sta_iface_names()
        ).unwrap()
        assert_equal(
            len(response.iface_names),
            0,
            "WifiChip should have returned an empty list of iface names",
        )

        proxy, server = Channel.create()
        asyncio.run(
            self.wifi_chip_proxy.create_sta_iface(iface=server.take())
        ).unwrap()
        wifi_sta_iface = fidl_wlanix.WifiStaIface.Client(proxy)

        response = asyncio.run(
            self.wifi_chip_proxy.get_sta_iface_names()
        ).unwrap()
        assert_equal(
            len(response.iface_names),
            1,
            "WifiChip should have returned the iface just created",
        )

        iface_name = response.iface_names[0]
        response = asyncio.run(wifi_sta_iface.get_name()).unwrap()
        assert_equal(
            iface_name,
            response.iface_name,
            "WifiStaIface returns a different name than WifiChip",
        )
        asyncio.run(
            self.wifi_chip_proxy.remove_sta_iface(iface_name=iface_name)
        ).unwrap()

        response = asyncio.run(
            self.wifi_chip_proxy.get_sta_iface_names()
        ).unwrap()
        assert_equal(
            len(response.iface_names),
            0,
            "WifiChip should no longer contain the iface just removed",
        )

        # TODO(https://fxbug.dev/365110075): Uncomment this check once wlanix supports
        # stopping a WifiStaIface after its iface is removed via WifiChip.RemoveStaIface
        # with assertRaises(Exception):
        #     asyncio.run(wifi_sta_iface.get_name()).unwrap())


if __name__ == "__main__":
    test_runner.main()
