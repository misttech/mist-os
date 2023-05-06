#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tracing capability default implementation."""

import os
import logging
import base64
from datetime import datetime
from typing import Any, Dict, List, Optional

from honeydew import errors
from honeydew.interfaces.affordances import tracing
from honeydew.transports import sl4f as sl4f_transport

_LOGGER: logging.Logger = logging.getLogger(__name__)

_SL4F_METHODS: Dict[str, str] = {
    "Initialize": "tracing_facade.Initialize",
    "Start": "tracing_facade.Start",
    "Stop": "tracing_facade.Stop",
    "Terminate": "tracing_facade.Terminate",
}


class TracingDefault(tracing.Tracing):
    """Default implementation for Tracing affordance.

    Args:
        device_name: Device name returned by `ffx target list`.
        sl4f: SL4F transport.
    """

    def __init__(self, device_name: str, sl4f: sl4f_transport.SL4F) -> None:
        self._name: str = device_name
        self._sl4f: sl4f_transport.SL4F = sl4f
        self._session_initialized: bool = False
        self._tracing_active: bool = False

    # List all the public methods in alphabetical order
    def initialize(
            self,
            categories: Optional[List[str]] = None,
            buffer_size: Optional[int] = None) -> None:
        """Initializes a trace sessions.

        Args:
            categories: list of categories to trace.
            buffer_size: buffer size to use in MB.

        Raises:
            errors.FuchsiaStateError: When Trace session is already initialized.
            errors.FuchsiaDeviceError: On failure.
        """
        if self._session_initialized:
            raise errors.FuchsiaStateError(
                f"Trace session is already initialized on {self._name}. Can be "
                "initialized only once")
        _LOGGER.info("Initializing trace session on '%s'", self._name)
        method_params: Dict[str, Any] = {}
        if categories:
            method_params["categories"] = categories
        if buffer_size:
            method_params["buffer_size"] = buffer_size
        self._sl4f.run(method=_SL4F_METHODS["Initialize"], params=method_params)
        self._session_initialized = True

    def start(self) -> None:
        """Starts tracing.

        Raises:
            errors.FuchsiaStateError: When Trace session is not initialized.
            errors.FuchsiaDeviceError: On failure.
        """
        if not self._session_initialized:
            raise errors.FuchsiaStateError(
                "Cannot start: Trace session is not "
                f"initialized on {self._name}")
        _LOGGER.info("Starting trace on '%s'", self._name)
        self._sl4f.run(method=_SL4F_METHODS["Start"])
        self._tracing_active = True

    def stop(self) -> None:
        """Stops the current trace.

        Raises:
            errors.FuchsiaStateError: When Trace session is not initialized or
                tracing not started.
            errors.FuchsiaDeviceError: On failure.
        """
        if not self._session_initialized:
            raise errors.FuchsiaStateError(
                "Cannot stop: Trace session is not "
                f"initialized on {self._name}")
        if not self._tracing_active:
            raise errors.FuchsiaStateError(
                f"Cannot stop: Trace not started on {self._name}")
        _LOGGER.info("Stopping trace on '%s'", self._name)
        self._sl4f.run(method=_SL4F_METHODS["Stop"])
        self._tracing_active = False

    def terminate(self) -> None:
        """Terminates the trace session without saving the trace.

        Raises:
            errors.FuchsiaStateError: When Trace session is not initialized.
            errors.FuchsiaDeviceError: On failure.
        """
        if not self._session_initialized:
            raise errors.FuchsiaStateError(
                "Cannot terminate: Trace session is "
                f"not initialized on {self._name}")

        _LOGGER.info("Terminating trace session on '%s'", self._name)
        self._sl4f.run(
            method=_SL4F_METHODS["Terminate"],
            params={"results_destination": "Ignore"})
        self._tracing_active = False
        self._session_initialized = False

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

        Raises:
            errors.FuchsiaStateError: When Trace session is not initialized.
            errors.FuchsiaDeviceError: On failure.
        """
        if not self._session_initialized:
            raise errors.FuchsiaStateError(
                "Cannot terminate: Trace session is "
                f"not initialized on {self._name}")

        _LOGGER.info("Terminating trace session on '%s'", self._name)
        resp: Dict[str, Any] = self._sl4f.run(method=_SL4F_METHODS["Terminate"])
        self._tracing_active = False
        self._session_initialized = False

        _LOGGER.info("Collecting trace on %s...", self._name)
        directory = os.path.abspath(directory)
        try:
            os.makedirs(directory)
        except FileExistsError:
            pass

        if not trace_file:
            timestamp: str = datetime.now().strftime("%Y-%m-%d-%I-%M-%S-%p")
            trace_file = f"trace_{self._name}_{timestamp}.fxt"
        trace_file_path: str = os.path.join(directory, trace_file)

        with open(trace_file_path, "wb") as trace_file_handle:
            trace_data: str = resp.get("result", {}).get("data", "")
            trace_file_handle.write(base64.b64decode(trace_data))

        _LOGGER.info("Trace downloaded at '%s'", trace_file_path)

        return trace_file_path
