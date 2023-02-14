#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains errors raised by HoneyDew."""

import logging

_LOGGER = logging.getLogger(__name__)


class HostCommandError(Exception):
    """Basic exception for errors raised by commands running on host machine.

    Attributes:
        err_code (int): numeric code of the error.
    """
    err_code = 1

    def __init__(self, msg):
        """Inits HostError with 'msg' (an error message string).

        Args:
            msg (str or Exception): an error message string or an Exception
            instance.

        Note: Additionally, logs 'msg' to debug log level file.
        """
        super().__init__(msg)
        _LOGGER.debug(repr(self))


class FfxCommandError(HostCommandError):
    """Exception for errors raised by ffx commands running on host machine.

    Attributes:
        err_code (int): numeric code of the error.
    """
    err_code = 2
