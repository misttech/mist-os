#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains errors raised by HoneyDew."""

import logging
from typing import Union

_LOGGER: logging.Logger = logging.getLogger(__name__)


class HoneyDewError(Exception):
    """Base exception for honeydew module.

    More specific exceptions will be created by inheriting from this exception.
    """

    def __init__(self, msg: Union[str, Exception]) -> None:
        """Inits HoneyDewError with 'msg' (an error message string).

        Args:
            msg: an error message string or an Exception instance.

        Note: Additionally, logs 'msg' to debug log level file.
        """
        super().__init__(msg)
        _LOGGER.debug(repr(self), exc_info=True)


class HoneyDewTimeoutError(HoneyDewError):
    """Exception for timeout based raised by HoneyDew."""


class FfxCommandError(HoneyDewError):
    """Exception for errors raised by ffx commands running on host machine."""


class HttpRequestError(HoneyDewError):
    """Exception for errors raised by HTTP requests running on host machine."""


class SSHCommandError(HoneyDewError):
    """Exception for errors raised by SSH commands running on host machine."""


class Sl4fError(HoneyDewError):
    """Exception for errors raised by SL4F requests."""


class FuchsiaControllerError(HoneyDewError):
    """Exception for errors raised by Fuchsia Controller requests."""


class FuchsiaStateError(HoneyDewError):
    """Exception for state errors."""


class FuchsiaDeviceError(HoneyDewError):
    """Base exception for errors raised by fuchsia device."""


class FastbootCommandError(HoneyDewError):
    """Exception for errors raised by Fastboot commands."""
