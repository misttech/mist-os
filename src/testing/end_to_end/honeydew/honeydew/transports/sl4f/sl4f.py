# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""ABC with methods for Host-(Fuchsia)Target interactions via SL4F."""

import abc
from collections.abc import Iterable
from typing import Any

from honeydew.utils import properties

DEFAULTS: dict[str, int] = {
    "ATTEMPTS": 3,
    "INTERVAL": 3,
}


class SL4F(abc.ABC):
    """ABC with methods for Host-(Fuchsia)Target interactions via SL4F."""

    @properties.DynamicProperty
    @abc.abstractmethod
    def url(self) -> str:
        """URL of the SL4F server.

        Returns:
            URL of the SL4F server.

        Raises:
            Sl4fError: On failure.
        """

    @abc.abstractmethod
    def check_connection(self) -> None:
        """Check SL4F connection between host and SL4F server running on device.

        Raises:
            Sl4fConnectionError
        """

    @abc.abstractmethod
    def run(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        timeout: float | None = None,
        attempts: int = DEFAULTS["ATTEMPTS"],
        interval: int = DEFAULTS["INTERVAL"],
        exceptions_to_skip: Iterable[type[Exception]] | None = None,
    ) -> dict[str, Any]:
        """Run the SL4F method on Fuchsia device and return the response.

        Args:
            method: SL4F method.
            params: Any optional params needed for method param.
            timeout: Timeout in seconds to wait for SL4F request to complete. By
                default, timeout is not set.
            attempts: number of attempts to try in case of a failure.
            interval: wait time in sec before each retry in case of a failure.
            exceptions_to_skip: Any non fatal exceptions for which retry will
                not be attempted and no error will be raised.

        Returns:
            SL4F command response returned by the Fuchsia device.
                Note: If SL4F command raises any exception specified in
                exceptions_to_skip then a empty dict will be returned.

        Raises:
            Sl4fError: On failure.
        """

    @abc.abstractmethod
    def start_server(self) -> None:
        """Starts the SL4F server on fuchsia device.

        Raises:
            Sl4fError: Failed to start the SL4F server.
        """
