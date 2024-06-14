# mypy: ignore-errors
#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth FIDL Server Implementations for Fuchsia Controller affordances"""

import logging

import fidl.fuchsia_bluetooth_sys as f_btsys_controller
from fidl import StopServer

from honeydew import errors

_LOGGER: logging.Logger = logging.getLogger(__name__)


class PairingDelegateImpl(f_btsys_controller.PairingDelegate.Server):
    """Pairing Delegate Server Implementation follows the FIDL SDK
    fuchsia.bluetooth.sys/pairing.fidl:PairingDelegate spec.
    """

    def on_pairing_request(
        self,
        pairing_start_request: f_btsys_controller.PairingDelegateOnPairingRequestRequest,
    ) -> f_btsys_controller.PairingDelegateOnPairingRequestResponse:
        """On Pairing Request implementation for Pairing Delegate Server

        Args:
            pairing_start_request: pairing request that Bluetooth stack received.

        Returns:
            response: pairing response to Bluetooth stack.
        """
        _LOGGER.info(
            "On Pairing Request method called with peer: %s",
            pairing_start_request.peer.id.value,
        )
        return f_btsys_controller.PairingDelegateOnPairingRequestResponse(
            accept=True, entered_passkey=0
        )

    def on_pairing_complete(
        self,
        pairing_complete_request: f_btsys_controller.PairingDelegateOnPairingCompleteRequest,
    ) -> None:
        """On Pairing Complete implementation for Pairing Delegate Server

        Args:
            pairing_complete_request: pairing response completion request from Bluetooth stack.

        Raises:
            BluetoothError: Pairing request failed to complete from the device.
            StopServer: Stop the FIDL Server since async will block execution indefinitely.
        """
        if not pairing_complete_request.success:
            raise errors.BluetoothError("Pairing request failed.")
        _LOGGER.info(
            "Pairing was successful. Calling StopServer to unblock execution."
        )
        raise StopServer
