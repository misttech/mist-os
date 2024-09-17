# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Tests of various (mostly hardcoded) properties returned from a WifiChip.
"""

import asyncio
import logging

import fidl.fuchsia_wlan_wlanix as fidl_wlanix
from antlion import base_test
from antlion.controllers import fuchsia_device
from fuchsia_controller_py import Channel
from honeydew.typing.custom_types import FidlEndpoint
from mobly import test_runner
from mobly.asserts import abort_class_if, assert_equal, assert_is_not


class WifiChipCorrectnessAndConsistencyTest(base_test.AntlionBaseTest):
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

    def test_chip_id_match(self) -> None:
        response = asyncio.run(self.wifi_chip_proxy.get_id()).unwrap()
        assert_equal(
            response.id,
            self.chip_id,
            "WifiChip itself returns a different id than chip_id",
        )

    # TODO(https://fxbug.dev/366028666): GetMode is hardcoded.
    def test_chip_mode_zeroed(
        self,
    ) -> None:
        response = asyncio.run(self.wifi_chip_proxy.get_mode()).unwrap()
        assert_equal(
            response.mode, 0, "WifiChip should have returned a hardcoded 0 mode"
        )

    # TODO(https://fxbug.dev/366027488): GetAvailableModes is hardcoded.
    def test_chip_available_modes_as_hardcoded(
        self,
    ) -> None:
        response = asyncio.run(
            self.wifi_chip_proxy.get_available_modes()
        ).unwrap()
        assert_equal(
            response,
            fidl_wlanix.WifiChipGetAvailableModesResponse(
                chip_modes=[
                    fidl_wlanix.ChipMode(
                        id=self.chip_id,
                        available_combinations=[
                            fidl_wlanix.ChipConcurrencyCombination(
                                limits=[
                                    fidl_wlanix.ChipConcurrencyCombinationLimit(
                                        types=[
                                            fidl_wlanix.IfaceConcurrencyType.STA
                                        ],
                                        max_ifaces=1,
                                    )
                                ]
                            )
                        ],
                    )
                ]
            ),
            "WifiChip returned a ChipMode different than the expected hardcoded response",
        )

    # TODO(https://fxbug.dev/366027491): GetCapabilities is hardcoded.
    def test_chip_capabilities_zeroed(self) -> None:
        response = asyncio.run(self.wifi_chip_proxy.get_capabilities()).unwrap()
        assert_equal(
            response.capabilities_mask,
            0,
            "WifiChip should have returned a hardcoded 0 capabilities_mask",
        )

    def test_chip_iface_names_empty(self) -> None:
        response = asyncio.run(
            self.wifi_chip_proxy.get_sta_iface_names()
        ).unwrap()
        assert_equal(
            len(response.iface_names),
            0,
            "WifiChip should have returned an empty list of iface names",
        )


if __name__ == "__main__":
    test_runner.main()
