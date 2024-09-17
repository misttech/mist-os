# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Test that the device has at least one WifiChip.
"""

import asyncio
import logging

import fidl.fuchsia_wlan_wlanix as fidl_wlanix
from antlion import base_test
from antlion.controllers import fuchsia_device
from fuchsia_controller_py import Channel
from honeydew.typing.custom_types import FidlEndpoint
from mobly import test_runner
from mobly.asserts import abort_class_if, assert_greater, assert_is_not


class WifiChipExistenceTest(base_test.AntlionBaseTest):
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

    def test_get_chip_ids(self) -> None:
        proxy, server = Channel.create()
        self.wlanix_proxy.get_wifi(wifi=server.take())
        wifi_proxy = fidl_wlanix.Wifi.Client(proxy)

        response = asyncio.run(wifi_proxy.get_chip_ids()).unwrap()
        assert_is_not(
            response.chip_ids,
            None,
            "Wifi.GetChipIds() response is missing a chip_ids value",
        )
        assert_greater(len(response.chip_ids), 0)


if __name__ == "__main__":
    test_runner.main()
