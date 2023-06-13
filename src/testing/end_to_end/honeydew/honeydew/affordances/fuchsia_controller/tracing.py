#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tracing affordance implementation using Fuchsia-Controller."""

from typing import List, Optional

from honeydew.interfaces.affordances import tracing


class Tracing(tracing.Tracing):
    """Tracing affordance implementation using Fuchsia-Controller."""

    # List all the public methods in alphabetical order
    def initialize(
            self,
            categories: Optional[List[str]] = None,
            buffer_size: Optional[int] = None) -> None:
        """Initializes a trace sessions.

        Args:
            categories: list of categories to trace.
            buffer_size: buffer size to use in MB.
        """
        raise NotImplementedError

    def start(self) -> None:
        """Starts tracing."""
        raise NotImplementedError

    def stop(self) -> None:
        """Stops the current trace."""
        raise NotImplementedError

    def terminate(self) -> None:
        """Terminates the trace session without saving the trace."""
        raise NotImplementedError

    def terminate_and_download(
            self, directory: str, trace_file: Optional[str] = None) -> str:
        """Terminates the trace session and downloads the trace data to the
            specified directory.

        Args:
            directory: Absolute path on the host where trace file will be
                saved. If this directory does not exist, this method will create
                it.

            trace_file: Name of the output trace file.
                If not provided, API will create a name using
                "trace_{device_name}_{'%Y-%m-%d-%I-%M-%S-%p'}" format.

        Returns:
            The path to the trace file.
        """
        raise NotImplementedError
