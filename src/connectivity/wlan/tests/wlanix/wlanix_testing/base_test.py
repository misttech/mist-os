# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Testing utilities for antlion tests of wlanix.
"""

import asyncio

import fidl.fuchsia_wlan_wlanix as fidl_wlanix
from antlion.controllers import fuchsia_device
from fuchsia_controller_py import Channel
from honeydew.typing.custom_types import FidlEndpoint
from mobly import base_test
from mobly.asserts import abort_class_if, assert_equal, assert_is_not


class WlanixBaseTestClass(base_test.BaseTestClass):
    wlanix_proxy: fidl_wlanix.Wlanix.Client

    def setup_class(self) -> None:
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


class WifiChipBaseTestClass(base_test.BaseTestClass):
    wlanix_proxy: fidl_wlanix.Wlanix.Client
    chip_id: int
    wifi_chip_proxy: fidl_wlanix.WifiChip.Client

    def setup_class(self) -> None:
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
