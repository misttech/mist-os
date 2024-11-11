# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Base test classes for antlion tests of WLAN core.
"""

import logging

logger = logging.getLogger(__name__)


import fidl.fuchsia_wlan_common as fidl_common
import fidl.fuchsia_wlan_device_service as fidl_svc
import fidl.fuchsia_wlan_sme as fidl_sme
from antlion import controllers
from antlion.controllers import fuchsia_device
from antlion.controllers.access_point import AccessPoint
from fuchsia_controller_py import Channel, ZxStatus
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod
from honeydew.typing.custom_types import FidlEndpoint
from mobly import base_test, signals
from mobly.asserts import abort_class_if, assert_equal, assert_is_not, fail
from mobly.records import TestResultRecord


class ConnectionBaseTestClass(AsyncAdapter, base_test.BaseTestClass):
    @asyncmethod
    async def setup_class(self) -> None:
        super().setup_class()
        self.pdu_devices = None

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

        self.device_monitor_proxy = fidl_svc.DeviceMonitor.Client(
            self.fuchsia_device.honeydew_fd.fuchsia_controller.connect_device_proxy(
                FidlEndpoint(
                    "core/wlandevicemonitor",
                    "fuchsia.wlan.device.service.DeviceMonitor",
                )
            )
        )
        response = await self.device_monitor_proxy.list_phys()
        assert_is_not(
            response.phy_list,
            None,
            "DeviceMonitor.ListPhys() response is missing a phy_list value",
        )
        assert_equal(
            len(response.phy_list),
            1,
            "DeviceMonitor.ListPhys() should return exactly one phy_id.",
        )

        self.phy_id = response.phy_list[0]
        logger.info(f"Using phy_id {self.phy_id}")

        response = await self.device_monitor_proxy.list_ifaces()
        if len(response.iface_list) > 0:
            fail(
                f"Found existing ifaces: {response.iface_list}. Every test suite should start with no ifaces."
            )

        req = fidl_svc.CreateIfaceRequest(
            phy_id=self.phy_id,
            role=fidl_common.WlanMacRole.CLIENT,
            sta_addr=[0, 0, 0, 0, 0, 0],
        )
        response = await self.device_monitor_proxy.create_iface(req=req)
        assert_equal(
            response.status,
            ZxStatus.ZX_OK,
            "DeviceMonitor.CreateIface() failed",
        )
        self.iface_id = response.resp.iface_id

        proxy, server = Channel.create()
        (
            await self.device_monitor_proxy.get_client_sme(
                iface_id=self.iface_id,
                sme_server=server.take(),
            )
        ).unwrap()
        self.client_sme_proxy = fidl_sme.ClientSme.Client(proxy)

        self.access_point().stop_all_aps()

    def teardown_test(self) -> None:
        # Maintain the invariant that every test starts with no access points.
        self.access_point().download_ap_logs(self.log_path)
        self.access_point().stop_all_aps()
        super().teardown_test()

    @asyncmethod
    async def teardown_class(self) -> None:
        logger.info(f"Destroying iface_id {self.iface_id}")
        req = fidl_svc.DestroyIfaceRequest(iface_id=self.iface_id)
        response = await self.device_monitor_proxy.destroy_iface(req=req)
        assert_equal(
            response.status,
            ZxStatus.ZX_OK,
            "DeviceMonitor.DestroyIface() failed",
        )
        self.iface_id = None

        # Save a snapshot after the entire test suite completes. This will be the only snapshot
        # if all tests succeeded.
        self.fuchsia_device.take_bug_report()
        self.access_point().stop_all_aps()
        super().teardown_class()

    def access_point(self) -> AccessPoint:
        if self.__access_point is None:
            raise RuntimeError("Connection tests require an access point.")
        return self.__access_point

    def on_fail(self, record: TestResultRecord) -> None:
        """A function that is executed upon a test failure.

        Args:
        record: A copy of the test record for this test, containing all information of
            the test execution including exception objects.
        """
        super().on_fail(record)

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
