# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Tracing affordance."""

import abc
import os
from collections.abc import Iterator
from contextlib import contextmanager


class Tracing(abc.ABC):
    """Abstract base class for Tracing affordance."""

    # List all the public methods
    @abc.abstractmethod
    def initialize(
        self,
        categories: list[str] | None = None,
        buffer_size: int | None = None,
        start_timeout_milliseconds: int | None = None,
    ) -> None:
        """Initializes a trace sessions.

        Args:
            categories: list of categories to trace.
            buffer_size: buffer size to use in MB.
            start_timeout_milliseconds: milliseconds to wait for trace providers
                to acknowledge that they've started tracing. NB: trace providers
                that don't ACK by this deadline may still emit tracing events
                starting at some later point.

        Raises:
            TracingStateError: When trace session is already initialized.
            TracingError: On FIDL communication failure.
        """

    @abc.abstractmethod
    def is_active(self) -> bool:
        """Checks if there is a currently active trace.

        Returns:
            True if the tracing is currently running, False otherwise.
        """

    @abc.abstractmethod
    def is_session_initialized(self) -> bool:
        """Checks if the session is initialized or not.

        Returns:
            True if the session is initialized, False otherwise.
        """

    @abc.abstractmethod
    def start(self) -> None:
        """Starts tracing.

        Raises:
           TracingStateError: When trace session is not initialized or
               already started.
           TracingError: On FIDL communication failure.
        """

    @abc.abstractmethod
    def stop(self) -> None:
        """Stops the current trace.

        Raises:
           TracingStateError: When trace session is not initialized or
               not started.
           TracingError: On FIDL communication failure.
        """

    @abc.abstractmethod
    def terminate(self) -> None:
        """Terminates the trace session.."""

    @abc.abstractmethod
    def terminate_and_download(
        self, directory: str, trace_file: str | None = None
    ) -> str:
        """Terminates the trace session and downloads the trace data to the
            specified directory.

        Args:
            directory: Absolute path on the host where trace file will be
                saved. If this directory does not exist, this method will create
                it.

            trace_file: Name of the output trace file.
                If not provided, API will create a name using
                "Trace_{device_name}_{'%Y-%m-%d-%I-%M-%S-%p'}" format.

        Returns:
            The path to the trace file.

         Raises:
            TracingStateError: When trace session is not initialized or
                already started.
        """

    @contextmanager
    def trace_session(
        self,
        categories: list[str] | None = None,
        buffer_size: int | None = None,
        download: bool = False,
        directory: str | None = None,
        trace_file: str | None = None,
    ) -> Iterator[None]:
        """Starts and captures trace data within a context and automatically
            cleans up trace sessions, making it easier to capture trace data.

        Args:
            categories: list of categories to trace.
            buffer_size: buffer size to use in MB.
            download: True to download trace, False to skip trace download.
            directory: Absolute path on the host where trace file will be
                saved. Only required if `download` is True.
            trace_file: Name of the output trace file. Only required if
                `download` is True.

         Raises:
            ValueError: If `download` is True but either `directory` or
                `trace_file` is not provided.
        """
        if not self.is_session_initialized():
            self.initialize(categories, buffer_size)
        try:
            self.start()
            yield
        finally:
            self.stop()
            if download:
                if directory is None or not os.path.isabs(directory):
                    raise ValueError(
                        "Provide a valid absoulte path to download the trace."
                    )
                if trace_file is None:
                    raise ValueError(
                        "Provide a valid trace file to download the trace."
                    )
                self.terminate_and_download(
                    directory=directory,
                    trace_file=trace_file,
                )
            else:
                self.terminate()
