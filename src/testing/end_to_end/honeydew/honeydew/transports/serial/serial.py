# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""ABC with methods for Host-(Fuchsia)Target interactions via Serial port."""

import abc


class Serial(abc.ABC):
    """ABC with methods for Host-(Fuchsia)Target interactions via Serial port."""

    @abc.abstractmethod
    def send(
        self,
        cmd: str,
    ) -> None:
        """Send command over serial port and immediately returns without any
        further checking if it ran successfully or not.

        Args:
            cmd: Command to run over serial port.

        Raises:
            SerialError: In case of failure.
        """
