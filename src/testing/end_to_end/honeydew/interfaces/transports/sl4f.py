#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for SL4F transport."""

import abc
from typing import Any, Dict, Iterable, Optional, Type


# pylint: disable=too-few-public-methods
class SL4F(abc.ABC):
    """Abstract base class for SL4F transport."""

    # List all the public methods in alphabetical order

    # pylint: disable=too-many-arguments
    @abc.abstractmethod
    def send_sl4f_command(
        self,
        method: str,
        params: Optional[Dict[str, Any]] = None,
        attempts: int = 3,
        interval: int = 3,
        exceptions_to_skip: Optional[Iterable[Type[Exception]]] = None
    ) -> Dict[str, Any]:
        """Sends SL4F command from host to Fuchsia device and returns the
        response.

        Args:
            method: SL4F method.
            params: Any optional params needed for method param.
            attempts: number of attempts to try in case of a failure.
            interval: wait time in sec before each retry in case of a failure.
            exceptions_to_skip: Any non fatal exceptions for which retry will
                not be attempted and no error will be raised.

        Returns:
            SL4F command response returned by the Fuchsia device.
                Note: If SL4F command raises any exception specified in
                exceptions_to_skip then a empty dict will be returned.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
