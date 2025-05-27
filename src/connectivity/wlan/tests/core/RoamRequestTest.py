# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Tests fulfillment of roam requests from the SME FIDL roam API.
"""
import logging

logger = logging.getLogger(__name__)

import asyncio
import time
from dataclasses import dataclass

import fidl_fuchsia_wlan_common as fidl_common
import fidl_fuchsia_wlan_common_security as fidl_security
import fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211
import fidl_fuchsia_wlan_sme as fidl_sme
from antlion import utils
from antlion.controllers.access_point import setup_ap
from antlion.controllers.ap_lib.hostapd_constants import (
    AP_DEFAULT_CHANNEL_2G,
    AP_DEFAULT_CHANNEL_5G,
    AP_SSID_LENGTH_2G,
    BandType,
)
from antlion.controllers.ap_lib.hostapd_security import Security, SecurityMode
from core_testing import base_test
from core_testing.handlers import ConnectTransactionEventHandler
from core_testing.ies import read_ssid
from fuchsia_controller_py.wrappers import asyncmethod
from honeydew.affordances.connectivity.wlan.utils.types import (
    MacAddress,
    QueryIfaceResponse,
)
from mobly import signals, test_runner
from mobly.asserts import abort_class_if, assert_equal, assert_true, fail

CONNECT_WAIT_TIME_SECONDS: int = 5
ROAM_RESULT_WAIT_TIME_SECONDS: int = 3
NEXT_TXN_WAIT_TIME_SECONDS: int = 1


@dataclass
class TestParams:
    dut_security_mode: SecurityMode
    origin_security_mode: SecurityMode
    origin_band: BandType
    target_security_mode: SecurityMode
    target_band: BandType


class RoamRequestTest(base_test.ConnectionBaseTestClass):
    """Tests fulfillment of roam requests from the SME FIDL roam API.

    Testbed Requirements:
    * One Fuchsia DUT
    * One AP

    Currently, this test only supports inter-band (2.4GHz/5GHz) roaming, as it is designed for
    standardized WLAN testbeds with a single AP. Multi-AP support is needed for intra-band
    roaming tests.
    """

    def pre_run(self) -> None:
        test_args: list[tuple[TestParams]] = [
            (
                TestParams(
                    dut_security_mode=SecurityMode.OPEN,
                    origin_security_mode=SecurityMode.OPEN,
                    origin_band=BandType.BAND_2G,
                    target_security_mode=SecurityMode.OPEN,
                    target_band=BandType.BAND_5G,
                ),
            ),
            (
                TestParams(
                    dut_security_mode=SecurityMode.OPEN,
                    origin_security_mode=SecurityMode.OPEN,
                    origin_band=BandType.BAND_5G,
                    target_security_mode=SecurityMode.OPEN,
                    target_band=BandType.BAND_2G,
                ),
            ),
        ]

        self.generate_tests(
            test_logic=self._test_logic,
            name_func=self.name_func,
            arg_sets=test_args,
        )

    def name_func(
        self,
        test_params: TestParams,
    ) -> str:
        return f"test_roam_request_{test_params.dut_security_mode}_dut_from_{test_params.origin_security_mode}_{test_params.origin_band.name}_to_{test_params.target_security_mode}_{test_params.target_band.name}"

    def setup_aps(
        self, test_params: TestParams
    ) -> tuple[str, Security, Security]:
        ssid = utils.rand_ascii_str(AP_SSID_LENGTH_2G)
        origin_ap_security_config = Security(test_params.origin_security_mode)
        target_ap_security_config = Security(test_params.target_security_mode)

        # Ensure the bands are a 2.4GHz and 5GHz pair. This test uses a single AP, and therefore
        # does not support the the same origin and target band.
        expected_bands = {BandType.BAND_2G, BandType.BAND_5G}
        actual_bands = {test_params.origin_band, test_params.target_band}
        abort_class_if(
            actual_bands != expected_bands,
            f"Test expects one 2.4GHz AP and one 5GHz AP. Got origin: {test_params.origin_band}, target {test_params.target_band}",
        )

        if test_params.origin_band == BandType.BAND_2G:
            security_2g = origin_ap_security_config
            security_5g = target_ap_security_config
        else:
            security_2g = target_ap_security_config
            security_5g = origin_ap_security_config

        # Setup 2.4GHz AP
        setup_ap(
            access_point=self.access_point(),
            profile_name="whirlwind",
            channel=AP_DEFAULT_CHANNEL_2G,
            ssid=ssid,
            security=security_2g,
        )

        # Setup 5GHz AP
        setup_ap(
            access_point=self.access_point(),
            profile_name="whirlwind",
            channel=AP_DEFAULT_CHANNEL_5G,
            ssid=ssid,
            security=security_5g,
        )

        return (ssid, origin_ap_security_config, target_ap_security_config)

    @asyncmethod
    async def _test_logic(
        self,
        test_params: TestParams,
    ) -> None:
        # Setup APs using test params
        (
            ssid,
            origin_ap_security_config,
            target_ap_security_config,
        ) = self.setup_aps(test_params)

        # Passive scan
        scan_results = (
            (
                await self.client_sme_proxy.scan_for_controller(
                    req=fidl_sme.ScanRequest(
                        passive=fidl_sme.PassiveScanRequest()
                    )
                )
            )
            .unwrap()
            .scan_results
        )
        if scan_results is None:
            raise signals.TestError(
                "ClientSme.ScanForController() response is missing scan_results"
            )

        # Parse out scanned BSSs from the test network
        bss_desc_2g = None
        bss_desc_5g = None
        for scan_result in scan_results:
            assert (
                scan_result.bss_description is not None
            ), "ScanResult is missing bss_description"
            assert (
                scan_result.bss_description.ies is not None
            ), "ScanResult.BssDescription is missing ies"
            scanned_ssid = read_ssid(bytes(scan_result.bss_description.ies))
            if scanned_ssid == ssid:
                channel = scan_result.bss_description.channel.primary
                if channel == AP_DEFAULT_CHANNEL_2G:
                    bss_desc_2g = scan_result.bss_description
                elif channel == AP_DEFAULT_CHANNEL_5G:
                    bss_desc_5g = scan_result.bss_description
                else:
                    raise signals.TestError(
                        f"BSS for test network SSID '{ssid}' found on unexpected primary channel: {channel}"
                    )

        # Verify there are two BSSs seen for the test network
        if bss_desc_2g is None:
            raise signals.TestError(
                f"Failed to see 2.4GHz BSS for SSID '{ssid}' in scan results"
            )
        if bss_desc_5g is None:
            raise signals.TestError(
                f"Failed to see 5GHz BSS for SSID '{ssid}' in scan results"
            )

        if test_params.origin_band == BandType.BAND_2G:
            origin_bss_desc = bss_desc_2g
            target_bss_desc = bss_desc_5g
        else:
            origin_bss_desc = bss_desc_5g
            target_bss_desc = bss_desc_2g

        with ConnectTransactionEventHandler() as ctx:
            txn_queue = ctx.txn_queue
            server = ctx.server

            # Setup credentials for the origin AP connection
            if test_params.origin_security_mode == SecurityMode.OPEN:
                protocol = fidl_security.Protocol.OPEN
                credentials = None
            elif test_params.origin_security_mode == SecurityMode.WPA2:
                protocol = fidl_security.Protocol.WPA2_PERSONAL
                credentials = fidl_security.Credentials(
                    wpa=fidl_security.WpaCredentials(
                        passphrase=list(
                            origin_ap_security_config.password.encode("ascii")
                        )
                    )
                )
            else:
                raise signals.TestError(
                    f"Unsupported security mode for origin AP: {test_params.origin_security_mode}"
                )

            # Send connect request for origin BSS
            connect_request = fidl_sme.ConnectRequest(
                ssid=list(ssid.encode("ascii")),
                bss_description=origin_bss_desc,
                multiple_bss_candidates=True,
                authentication=fidl_security.Authentication(
                    protocol=protocol,
                    credentials=credentials,
                ),
                deprecated_scan_type=fidl_common.ScanType.PASSIVE,
            )
            logger.info(f"ConnectRequest: {connect_request!r}")
            self.client_sme_proxy.connect(
                req=connect_request, txn=server.take()
            )

            # Verify a successful connect result is received
            try:
                next_txn = await asyncio.wait_for(
                    txn_queue.get(), timeout=CONNECT_WAIT_TIME_SECONDS
                )
            except TimeoutError:
                raise signals.TestError(
                    f"Timed out after {CONNECT_WAIT_TIME_SECONDS} seconds awaiting a connect result"
                )

            assert_equal(
                next_txn,
                fidl_sme.ConnectTransactionOnConnectResultRequest(
                    result=fidl_sme.ConnectResult(
                        code=fidl_ieee80211.StatusCode.SUCCESS,
                        is_credential_rejected=False,
                        is_reconnect=False,
                    )
                ),
            )
            if not txn_queue.empty():
                raise signals.TestError(
                    "Unexpectedly received additional callback messages after connect result."
                )

            # Verify that DUT is actually associated (as seen from AP).
            client_mac = await self._get_client_mac()
            if test_params.origin_band == BandType.BAND_2G:
                origin_iface = self.access_point().wlan_2g
                target_iface = self.access_point().wlan_5g
            else:
                origin_iface = self.access_point().wlan_5g
                target_iface = self.access_point().wlan_2g

            if not self.access_point().sta_authenticated(
                origin_iface, client_mac
            ):
                raise signals.TestError(
                    f"DUT is not authenticated on the {test_params.origin_band} band"
                )

            if not self.access_point().sta_associated(origin_iface, client_mac):
                raise signals.TestError(
                    f"DUT is not associated on the {test_params.origin_band} band"
                )

            if not self.access_point().sta_authorized(origin_iface, client_mac):
                raise signals.TestError(
                    f"DUT is not authorized on the {test_params.origin_band} band"
                )

            # Send a roam request for target BSS. From this point, failed assert calls are relevant
            # to the roam attempt.
            roam_request = fidl_sme.RoamRequest(bss_description=target_bss_desc)
            logger.info(f"RoamRequest: {roam_request!r}")
            self.client_sme_proxy.roam(req=roam_request)

            # Verify a successful roam result is received. Filter out any signal reports. Waits up
            # to NEXT_TXN_WAIT_TIME_SECONDS for the next txn, and up to
            # ROAM_RESULT_WAIT_TIME_SECONDS for a roam result.
            start_time = time.time()
            while time.time() < start_time + ROAM_RESULT_WAIT_TIME_SECONDS:
                # Wait for the next txn. If next txn is:
                # - OnRoamResultRequest: verify roam result
                # - OnSignalReportRequest: ignore, and continue waiting (up to ROAM_RESULT_WAIT_TIME_SECONDS)
                # - None | something else: fail and exit
                next_txn = await asyncio.wait_for(
                    txn_queue.get(), timeout=NEXT_TXN_WAIT_TIME_SECONDS
                )
                if next_txn is None:
                    fail(
                        f"Failed to receive the next transaction connection (OnRoamResultRequest or otherwise) within {NEXT_TXN_WAIT_TIME_SECONDS} seconds."
                    )
                match next_txn:
                    case txn if isinstance(
                        txn, fidl_sme.ConnectTransactionOnSignalReportRequest
                    ):
                        # Ignore any signal reports
                        logger.info(f"Ignoring signal report: {txn}")
                        continue
                    case txn if isinstance(
                        txn, fidl_sme.ConnectTransactionOnRoamResultRequest
                    ):
                        # Verify roam result
                        logger.info(
                            f"ConnectTransactionOnRoamResultRequest received: {next_txn}"
                        )
                        assert_equal(
                            next_txn,
                            fidl_sme.ConnectTransactionOnRoamResultRequest(
                                result=fidl_sme.RoamResult(
                                    bssid=target_bss_desc.bssid,
                                    status_code=fidl_ieee80211.StatusCode.SUCCESS,
                                    original_association_maintained=False,
                                    bss_description=target_bss_desc,
                                    disconnect_info=None,
                                    is_credential_rejected=False,
                                )
                            ),
                        )
                        # Verify DUT is connected to the AP using the target interface
                        assert_true(
                            self.access_point().sta_authenticated(
                                target_iface, client_mac
                            ),
                            f"DUT is not authenticated on the {test_params.target_band} band",
                        )
                        assert_true(
                            self.access_point().sta_associated(
                                target_iface, client_mac
                            ),
                            f"DUT is not associated on the {test_params.target_band} band",
                        )
                        assert_true(
                            self.access_point().sta_authorized(
                                target_iface, client_mac
                            ),
                            f"DUT is not 802.1X authorized on the {test_params.target_band} band",
                        )
                        break
                    case _:
                        fail(
                            f"Unexpected transaction received while waiting for roam result: {next_txn}"
                        )
            else:
                fail(
                    f"Never received a roam result for target BSSID {target_bss_desc.bssid} within the {ROAM_RESULT_WAIT_TIME_SECONDS} second timeout period."
                )

    async def _get_client_mac(self) -> str:
        """Get the MAC address of the DUT client interface.

        Returns:
            str, MAC address of the DUT client interface.
        Raises:
            RuntimeError if there is no DUT client interface or if the DUT interface query fails.
        """
        if self.iface_id is not None:
            try:
                query_iface_response = (
                    await self.device_monitor_proxy.query_iface(
                        iface_id=self.iface_id
                    )
                ).unwrap()
            except Exception as e:
                raise RuntimeError(
                    f"DeviceMonitor.QueryIface() error: {e}"
                ) from e
            resp = QueryIfaceResponse.from_fidl(query_iface_response.resp)
            mac_addr = MacAddress.from_bytes(bytes(resp.sta_addr))
            return str(mac_addr)
        raise RuntimeError("Interface id is not set.")


if __name__ == "__main__":
    test_runner.main()
