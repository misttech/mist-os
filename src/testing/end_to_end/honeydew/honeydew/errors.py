#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains errors raised by Honeydew."""

import logging

_LOGGER: logging.Logger = logging.getLogger(__name__)


class HoneydewError(Exception):
    """Base exception for Honeydew module.

    More specific exceptions will be created by inheriting from this exception.
    """

    def __init__(self, msg: str | Exception) -> None:
        """Inits HoneydewError with 'msg' (an error message string).

        Args:
            msg: an error message string or an Exception instance.

        Note: Additionally, logs 'msg' to debug log level file.
        """
        super().__init__(msg)
        _LOGGER.debug(repr(self), exc_info=True)


class TransportError(HoneydewError):
    """Exception for errors raised by Honeydew transports."""


class HttpRequestError(TransportError):
    """Exception for errors raised by HTTP requests running on host machine."""


class Sl4fError(TransportError):
    """Exception for errors raised by SL4F requests."""


class SSHCommandError(TransportError):
    """Exception for errors raised by SSH commands running on host machine."""


class FfxCommandError(TransportError):
    """Exception for errors raised by ffx commands running on host machine."""


class FuchsiaControllerError(TransportError):
    """Exception for errors raised by Fuchsia Controller requests."""


class FastbootCommandError(HoneydewError):
    """Exception for errors raised by Fastboot commands."""


class TransportConnectionError(HoneydewError):
    """Raised when transport's check_connection fails."""


class Sl4fConnectionError(TransportConnectionError):
    """Raised when FFX transport's check_connection fails."""


class SshConnectionError(TransportConnectionError):
    """Raised when SSH transport's check_connection fails."""


class FfxConnectionError(TransportConnectionError):
    """Raised when FFX transport's check_connection fails."""


class FuchsiaControllerConnectionError(TransportConnectionError):
    """Raised when Fuchsia-Controller transport's check_connection fails."""


class FastbootConnectionError(TransportConnectionError):
    """Raised when Fastboot transport's check_connection fails."""


class FfxConfigError(HoneydewError):
    """Raised by ffx.FfxConfig class."""


class HoneydewTimeoutError(HoneydewError):
    """Exception for timeout based raised by Honeydew."""


class HoneydewRtcError(HoneydewError):
    """Raised by rtc.Rtc class."""


class FuchsiaStateError(HoneydewError):
    """Exception for state errors."""


class FuchsiaDeviceError(HoneydewError):
    """Base exception for errors raised by fuchsia device."""


class SessionError(HoneydewError):
    """Exception for errors raised by Session."""


class DeviceNotConnectedError(HoneydewError):
    """Exception to be raised when device is not connected to host."""


class SystemPowerStateControllerError(HoneydewError):
    """Exception to be raised by SystemPowerStateController affordance."""


class NotSupportedError(HoneydewError):
    """Exception to be raised if an operation is not yet supported by
    underlying Fuchsia platform."""


class StarnixError(HoneydewError):
    """Exception to be raised if a starnix operation fails."""
