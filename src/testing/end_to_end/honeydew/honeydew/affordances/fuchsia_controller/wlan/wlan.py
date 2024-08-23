# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""WLAN affordance implementation using Fuchsia Controller."""

import asyncio
import logging

import fidl.fuchsia_location_namedplace as f_location_namedplace
import fidl.fuchsia_wlan_device_service as f_wlan_device_service
import fidl.fuchsia_wlan_sme as f_wlan_sme
from fuchsia_controller_py import Channel, ZxStatus

from honeydew import errors
from honeydew.interfaces.affordances.wlan import wlan
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.custom_types import FidlEndpoint
from honeydew.typing.wlan import (
    BssDescription,
    ClientStatusResponse,
    CountryCode,
    MacAddress,
    QueryIfaceResponse,
    WlanMacRole,
)

# List of required FIDLs for the WLAN Fuchsia Controller affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.location.namedplace",
    "fuchsia.wlan.device.service",
    "fuchsia.wlan.phyimpl",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)

# Fuchsia Controller proxies
_DEVICE_MONITOR_PROXY = FidlEndpoint(
    "core/wlandevicemonitor", "fuchsia.wlan.device.service.DeviceMonitor"
)
_REGULATORY_REGION_CONFIGURATOR_PROXY = FidlEndpoint(
    "core/regulatory_region",
    "fuchsia.location.namedplace.RegulatoryRegionConfigurator",
)


class Wlan(wlan.Wlan):
    """WLAN affordance implemented with Fuchsia Controller."""

    def __init__(
        self,
        device_name: str,
        ffx: ffx_transport.FFX,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
    ) -> None:
        """Create a WLAN Fuchsia Controller affordance.

        Args:
            device_name: Device name returned by `ffx target list`.
            ffx: FFX transport.
            fuchsia_controller: Fuchsia Controller transport.
            reboot_affordance: Object that implements RebootCapableDevice.
        """
        self._verify_supported(device_name, ffx)

        self._fc_transport = fuchsia_controller
        self._reboot_affordance = reboot_affordance

        self._connect_proxy()
        self._reboot_affordance.register_for_on_device_boot(self._connect_proxy)

    def _verify_supported(self, device: str, ffx: ffx_transport.FFX) -> None:
        """Check if WLAN is supported on the DUT.

        Args:
            device: Device name returned by `ffx target list`.
            ffx: FFX transport

        Raises:
            NotSupportedError: A required component capability is not available.
        """
        for capability in _REQUIRED_CAPABILITIES:
            # TODO(http://b/359342196): This is a maintenance burden; find a
            # better way to detect FIDL component capabilities.
            if capability not in ffx.run(
                ["component", "capability", capability]
            ):
                _LOGGER.warning(
                    "All available WLAN component capabilities:\n%s",
                    ffx.run(["component", "capability", "fuchsia.wlan"]),
                )
                raise errors.NotSupportedError(
                    f'Component capability "{capability}" not exposed by device '
                    f"{device}; this build of Fuchsia does not support the "
                    "WLAN FC affordance."
                )

    def _connect_proxy(self) -> None:
        """Re-initializes connection to the WLAN stack."""
        self._device_monitor_proxy = f_wlan_device_service.DeviceMonitor.Client(
            self._fc_transport.connect_device_proxy(_DEVICE_MONITOR_PROXY)
        )
        self._regulatory_region_configurator = (
            f_location_namedplace.RegulatoryRegionConfigurator.Client(
                self._fc_transport.connect_device_proxy(
                    _REGULATORY_REGION_CONFIGURATOR_PROXY
                )
            )
        )

    def connect(
        self,
        ssid: str,
        password: str | None,
        bss_desc: BssDescription,
        # TODO(http://b/324949922): Uncomment once the WLAN affordance API is
        # changed to include the desired authentication in every call to
        # connect().
        # authentication: Authentication,
    ) -> bool:
        """Trigger connection to a network.

        Args:
            ssid: The network to connect to.
            password: The password for the network.
            bss_desc: The basic service set for target network.

        Returns:
            True on success otherwise false.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            NetworkInterfaceNotFoundError: No client WLAN interface found.
        """
        # TODO(http://b/324949922): Uncomment once the WLAN affordance API is
        # changed to include the desired authentication in every call to
        # connect().
        # TODO(http://b/324949815): Implement server to ConnectTransaction
        raise NotImplementedError()

    def create_iface(
        self, phy_id: int, role: WlanMacRole, sta_addr: str | None = None
    ) -> int:
        """Create a new WLAN interface.

        Args:
            phy_id: The iface ID.
            role: The role of the new iface.
            sta_addr: MAC address for softAP iface.

        Returns:
            Iface id of newly created interface.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            ValueError: Invalid MAC address
        """
        if sta_addr is None:
            sta_addr = "00:00:00:00:00:00"
            _LOGGER.warning(
                "No MAC provided in args of create_iface, using %s", sta_addr
            )

        req = f_wlan_device_service.CreateIfaceRequest(
            phy_id=phy_id,
            role=role.to_fidl(),
            sta_addr=MacAddress(sta_addr).bytes(),
        )
        try:
            create_iface = asyncio.run(
                self._device_monitor_proxy.create_iface(req=req)
            )
            if create_iface.status != ZxStatus.ZX_OK:
                raise ZxStatus(create_iface.status)
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"DeviceMonitor.CreateIface() error {status}"
            ) from status

        return create_iface.resp.iface_id

    def destroy_iface(self, iface_id: int) -> None:
        """Destroy WLAN interface by ID.

        Args:
            iface_id: The interface to destroy.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        req = f_wlan_device_service.DestroyIfaceRequest(iface_id=iface_id)
        try:
            destroy_iface = asyncio.run(
                self._device_monitor_proxy.destroy_iface(req=req)
            )
            if destroy_iface.status != ZxStatus.ZX_OK:
                raise ZxStatus(destroy_iface.status)
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"DeviceMonitor.DestroyIface() error {status}"
            ) from status

    def disconnect(self) -> None:
        """Disconnect all client WLAN connections.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        iface_ids = self.get_iface_id_list()

        for iface_id in iface_ids:
            info = self.query_iface(iface_id)
            if info.role == WlanMacRole.CLIENT:
                sme = self._get_client_sme(iface_id)
                try:
                    asyncio.run(
                        sme.disconnect(
                            reason=f_wlan_sme.UserDisconnectReason.WLAN_SERVICE_UTIL_TESTING,
                        )
                    )
                except ZxStatus as status:
                    raise errors.HoneydewWlanError(
                        f"SmeClient.Disconnect() error {status}"
                    ) from status

    def get_country(self, phy_id: int) -> CountryCode:
        """Queries the currently configured country code from phy `phy_id`.

        Args:
            phy_id: A phy id that is present on the device.

        Returns:
            The currently configured country code from `phy_id`.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        try:
            get_country = asyncio.run(
                self._device_monitor_proxy.get_country(phy_id=phy_id)
            )
            if get_country.err is not None:
                raise ZxStatus(get_country.err)
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"DeviceMonitor.GetCountry() error {status}"
            ) from status

        return CountryCode(
            bytes(get_country.response.resp.alpha2).decode("utf-8")
        )

    def get_iface_id_list(self) -> list[int]:
        """Get list of wlan iface IDs on device.

        Returns:
            A list of wlan iface IDs that are present on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        try:
            resp = asyncio.run(self._device_monitor_proxy.list_ifaces())
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"DeviceMonitor.ListIfaces() error {status}"
            ) from status
        return resp.iface_list

    def get_phy_id_list(self) -> list[int]:
        """Get list of phy ids on device.

        Returns:
            A list of phy ids that is present on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        try:
            resp = asyncio.run(self._device_monitor_proxy.list_phys())
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"DeviceMonitor.ListPhys() error {status}"
            ) from status
        return resp.phy_list

    def query_iface(self, iface_id: int) -> QueryIfaceResponse:
        """Retrieves interface info for given wlan iface id.

        Args:
            iface_id: The wlan interface id to get info from.

        Returns:
            QueryIfaceResponseWrapper from the SL4F server.

        Raises:
            HoneydewWlanError: DeviceMonitor.QueryIface error
        """
        try:
            query = asyncio.run(
                self._device_monitor_proxy.query_iface(iface_id=iface_id)
            )
            if query.err is not None:
                raise query.err
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"DeviceMonitor.QueryIface() error {status}"
            ) from status
        return QueryIfaceResponse.from_fidl(query.response.resp)

    def scan_for_bss_info(self) -> dict[str, list[BssDescription]]:
        """Scans and returns BSS info.

        Returns:
            A dict mapping each seen SSID to a list of BSS Description IE
            blocks, one for each BSS observed in the network

        Raises:
            HoneydewWlanError: Error from WLAN stack
            NetworkInterfaceNotFoundError: No client WLAN interface found.
        """
        iface_id = self._get_first_sme(WlanMacRole.CLIENT)
        client_sme = self._get_client_sme(iface_id)

        # Perform a passive scan
        req = f_wlan_sme.ScanRequest()
        req.passive = f_wlan_sme.PassiveScanRequest()

        try:
            scan = asyncio.run(client_sme.scan_for_controller(req=req))
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"ClientSme.ScanForController() error {status}"
            ) from status

        if scan.err is not None:
            raise errors.HoneydewWlanError(
                f"ClientSme.ScanForController() ScanErrorCode {scan.err}"
            )

        results: dict[str, list[BssDescription]] = {}

        for scan_result in scan.response.scan_results:
            desc = BssDescription.from_fidl(scan_result.bss_description)
            ssid = desc.ssid()
            if ssid:
                if ssid in results:
                    results[ssid].append(desc)
                else:
                    results[ssid] = [desc]
            else:
                _LOGGER.warning(
                    "Scan result does not contain SSID: %s", scan_result
                )

        return results

    def _get_first_sme(self, role: WlanMacRole) -> int:
        """Find a WLAN interface running with the specified role.

        Args:
            role: Desired mode of the WLAN interface.

        Raises:
            NetworkInterfaceNotFoundError: No WLAN interface found running with the
                specified role.

        Returns:
            ID of the first WLAN interface found running with the specified
            role. The others are discarded.
        """
        iface_ids = self.get_iface_id_list()
        if len(iface_ids) == 0:
            raise errors.NetworkInterfaceNotFoundError(
                "No WLAN interface found"
            )

        for iface_id in iface_ids:
            info = self.query_iface(iface_id)
            if info.role == role:
                return iface_id

        raise errors.NetworkInterfaceNotFoundError(
            f"WLAN interface with role {role} not found"
        )

    def _get_client_sme(self, iface_id: int) -> f_wlan_sme.ClientSme.Client:
        """Get a handle to ClientSme for performing SME actions.

        Args:
            iface_id: The wlan interface id to connect with

        Raises:
            HoneydewWlanError: DeviceMonitor.QueryIface error

        Returns:
            Client-side handle to the fuchsia.wlan.sme.ClientSme protocol, used
            for performing driver-layer actions on the underlying WLAN hardware.
        """
        client, server = Channel.create()
        sme_client = f_wlan_sme.ClientSme.Client(client)

        try:
            asyncio.run(
                self._device_monitor_proxy.get_client_sme(
                    iface_id=iface_id, sme_server=server.take()
                )
            )
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"DeviceMonitor.GetClientSme() error {status}"
            ) from status

        return sme_client

    def set_region(self, region_code: str) -> None:
        """Set regulatory region.

        Args:
            region_code: 2-byte ASCII string.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            TypeError: Invalid region_code format
        """
        if len(region_code) != 2:
            raise TypeError(
                f'Expected region_code to be length 2, got "{region_code}"'
            )

        try:
            self._regulatory_region_configurator.set_region(region=region_code)
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"RegulatoryRegionConfigurator.SetRegion() error {status}"
            ) from status

    def status(self) -> ClientStatusResponse:
        """Request connection status

        Returns:
            ClientStatusResponse which can be any one of three  things:
            ClientStatusConnected, ClientStatusConnecting, ClientStatusIdle.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            NetworkInterfaceNotFoundError: No client WLAN interface found.
            TypeError: If any of the return values are not of the expected type.
        """
        iface_id = self._get_first_sme(WlanMacRole.CLIENT)
        sme = self._get_client_sme(iface_id)

        try:
            resp = asyncio.run(sme.status())
        except ZxStatus as status:
            raise errors.HoneydewWlanError(
                f"ClientSme.Status() error {status}"
            ) from status

        return ClientStatusResponse.from_fidl(resp.resp)
