# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Testing utilities for antlion tests of wlanix.
"""

import asyncio
from typing import Any

import fidl.fuchsia_wlan_wlanix as fidl_wlanix
from antlion import controllers
from antlion.controllers import fuchsia_device
from antlion.controllers.access_point import AccessPoint
from antlion.controllers.ap_lib import hostapd_constants
from antlion.controllers.packet_capture import PacketCapture
from antlion.controllers.pdu import PduDevice
from antlion.test_utils.wifi import wifi_test_utils as wutils
from fuchsia_controller_py import Channel
from honeydew.typing.custom_types import FidlEndpoint
from mobly import base_test, signals
from mobly.asserts import abort_class_if, assert_equal, assert_is_not
from mobly.records import TestResultRecord


class WlanixBaseTestClass(base_test.BaseTestClass):
    wlanix_proxy: fidl_wlanix.Wlanix.Client

    def setup_class(self) -> None:
        fuchsia_devices = self.register_controller(fuchsia_device)

        abort_class_if(
            len(fuchsia_devices) != 1, "Requires exactly one Fuchsia device"
        )
        self.fuchsia_device = fuchsia_devices[0]
        abort_class_if(
            not hasattr(self.fuchsia_device, "honeydew_fd")
            or self.fuchsia_device.honeydew_fd is None,
            "Requires a Honeydew-enabled FuchsiaDevice",
        )

        self.wlanix_proxy = fidl_wlanix.Wlanix.Client(
            self.fuchsia_device.honeydew_fd.fuchsia_controller.connect_device_proxy(
                FidlEndpoint("core/wlanix", "fuchsia.wlan.wlanix.Wlanix")
            )
        )


class WifiChipBaseTestClass(WlanixBaseTestClass):
    chip_id: int
    wifi_chip_proxy: fidl_wlanix.WifiChip.Client
    allow_ifaces_between_tests: bool

    def __init__(
        self,
        *args: Any,
        allow_ifaces_between_tests: bool = False,
        **kwargs: Any,
    ) -> None:
        super().__init__(*args, **kwargs)
        self.allow_ifaces_between_tests = allow_ifaces_between_tests

    def setup_class(self) -> None:
        super().setup_class()

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
        if not self.allow_ifaces_between_tests:
            response = asyncio.run(
                self.wifi_chip_proxy.get_sta_iface_names()
            ).unwrap()
            assert_equal(
                len(response.iface_names),
                0,
                "Every test should end with no ifaces.",
            )


class IfaceBaseTestClass(WifiChipBaseTestClass):
    wifi_sta_iface_proxy: fidl_wlanix.WifiStaIface.Client
    supplicant_sta_iface_proxy: fidl_wlanix.SupplicantStaIface.Client
    iface_name: str

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__(*args, allow_ifaces_between_tests=True, **kwargs)

    def setup_class(self) -> None:
        super().setup_class()

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
        self.wifi_sta_iface_proxy = fidl_wlanix.WifiStaIface.Client(proxy)
        self.iface_name = (
            asyncio.run(self.wifi_sta_iface_proxy.get_name())
            .unwrap()
            .iface_name
        )

        proxy, server = Channel.create()
        self.wlanix_proxy.get_supplicant(supplicant=server.take())
        supplicant_proxy = fidl_wlanix.Supplicant.Client(proxy)

        proxy, server = Channel.create()
        supplicant_proxy.add_sta_interface(
            iface=server.take(),
            iface_name=self.iface_name,
        )
        self.supplicant_sta_iface_proxy = fidl_wlanix.SupplicantStaIface.Client(
            proxy
        )

    def teardown_class(self) -> None:
        async def destroy_iface() -> None:
            (
                await self.wifi_chip_proxy.remove_sta_iface(
                    iface_name=self.iface_name
                )
            ).unwrap()

            response = (
                await self.wifi_chip_proxy.get_sta_iface_names()
            ).unwrap()
            assert_equal(
                len(response.iface_names),
                0,
                "WifiChip should no longer contain the iface just removed",
            )

        asyncio.run(destroy_iface())


class ConnectionBaseTestClass(IfaceBaseTestClass):
    __access_point: AccessPoint | None
    pdu_devices: list[PduDevice] | None
    packet_capture: list[PacketCapture] | None
    packet_logger: PacketCapture | None
    packet_log_pid: dict[str, int] | None
    nl80211_proxy: fidl_wlanix.Nl80211.Client

    def access_point(self) -> AccessPoint:
        if self.__access_point is None:
            raise RuntimeError("Connection tests require an access point.")
        return self.__access_point

    def setup_class(self) -> None:
        super().setup_class()
        self.pdu_devices = None
        self.packet_capture = None
        self.packet_logger = None
        self.packet_log_pid = {}

        proxy, server = Channel.create()
        self.wlanix_proxy.get_nl80211(nl80211=server.take())
        self.nl80211_proxy = fidl_wlanix.Nl80211.Client(proxy)

        access_points = self.register_controller(
            controllers.access_point, min_number=1
        )
        if access_points is None or len(access_points) == 0:
            raise signals.TestAbortClass("Requires at least one access point")
        self.__access_point = access_points[0]

        self.pdu_devices = self.register_controller(
            controllers.pdu,
            # TODO(https://fxbug.dev/369159708) This should be required, but it inhibits
            # local testing when a PDU is not present.
            required=False,
        )

        self.packet_capture = self.register_controller(
            controllers.packet_capture, required=False
        )
        if self.packet_capture and len(self.packet_capture) > 0:
            self.packet_logger = self.packet_capture[0]
            self.packet_logger.configure_monitor_mode(
                "2G", hostapd_constants.AP_DEFAULT_CHANNEL_2G
            )
            self.packet_logger.configure_monitor_mode(
                "5G", hostapd_constants.AP_DEFAULT_CHANNEL_5G
            )

        self.access_point().stop_all_aps()

    def setup_test(self) -> None:
        super().setup_test()

        # Start a packet capture that can be used for debugging tests upon success or failure.
        if self.packet_logger:
            self.packet_log_pid = wutils.start_pcap(
                self.packet_logger, "dual", self.current_test_info.name
            )

    def teardown_test(self) -> None:
        # Save a packet capture for debugging
        if self.packet_logger and self.packet_log_pid:
            wutils.stop_pcap(
                self.packet_logger, self.packet_log_pid, test_status=True
            )
            self.packet_log_pid = {}

        # Maintain the invariant that every test starts with no access points.
        self.access_point().download_ap_logs(self.log_path)
        self.access_point().stop_all_aps()
        super().teardown_test()

    def teardown_class(self) -> None:
        # Save a snapshot after the entire test suite completes. This will be the only snapshot
        # if all tests succeeded.
        self.fuchsia_device.take_bug_report()
        super().teardown_class()

    def on_fail(self, record: TestResultRecord) -> None:
        """A function that is executed upon a test failure.

        Args:
        record: A copy of the test record for this test, containing all information of
            the test execution including exception objects.
        """
        super().on_fail(record)
        # Save a packet capture for debugging
        if self.packet_logger and self.packet_log_pid:
            wutils.stop_pcap(
                self.packet_logger, self.packet_log_pid, test_status=False
            )
            self.packet_log_pid = {}

        # Save a snapshot for debugging
        if (
            hasattr(self.fuchsia_device, "take_bug_report_on_fail")
            and self.fuchsia_device.take_bug_report_on_fail
        ):
            self.fuchsia_device.take_bug_report()

        # Rebooting on failure can avoid unintentionally propagating
        # a single test failure to tests that follow.
        if (
            hasattr(self.fuchsia_device, "hard_reboot_on_fail")
            and self.fuchsia_device.hard_reboot_on_fail
            and self.pdu_devices
        ):
            self.fuchsia_device.reboot(
                reboot_type="hard", testbed_pdus=self.pdu_devices
            )

        # Maintain the invariant that every test starts with no access points.
        self.access_point().stop_all_aps()
