#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains errors raised by HoneyDew."""

import logging

_LOGGER: logging.Logger = logging.getLogger(__name__)


class HoneyDewError(Exception):
    """Base exception for honeydew module.

    More specific exceptions will be created by inheriting from this exception.
    """

    def __init__(self, msg: str | Exception) -> None:
        """Inits HoneyDewError with 'msg' (an error message string).

        Args:
            msg: an error message string or an Exception instance.

        Note: Additionally, logs 'msg' to debug log level file.
        """
        super().__init__(msg)
        _LOGGER.debug(repr(self), exc_info=True)


class TransportError(HoneyDewError):
    """Exception for errors raised by honeydew transports."""


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


class FastbootCommandError(HoneyDewError):
    """Exception for errors raised by Fastboot commands."""


class TransportConnectionError(HoneyDewError):
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


class FfxConfigError(HoneyDewError):
    """Raised by ffx.FfxConfig class."""


class HoneyDewTimeoutError(HoneyDewError):
    """Exception for timeout based raised by HoneyDew."""


class FuchsiaStateError(HoneyDewError):
    """Exception for state errors."""


class FuchsiaDeviceError(HoneyDewError):
    """Base exception for errors raised by fuchsia device."""


class SessionError(HoneyDewError):
    """Exception for errors raised by Session."""


class DeviceNotConnectedError(HoneyDewError):
    """Exception to be raised when device is not connected to host."""
