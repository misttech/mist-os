#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tracing affordance implementation using Fuchsia-Controller."""

import asyncio
from datetime import datetime
import logging
import os
from typing import Dict, List, Optional

import fidl.fuchsia_tracing as f_tracing
import fidl.fuchsia_tracing_controller as f_tracingcontroller
import fuchsia_controller_py as fc

from honeydew import custom_types
from honeydew import errors
from honeydew.interfaces.affordances import tracing
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import fuchsia_controller as fc_transport

_FC_PROXIES: Dict[str, custom_types.FidlEndpoint] = {
    "TracingController":
        custom_types.FidlEndpoint(
            "/core/trace_manager", "fuchsia.tracing.controller.Controller"),
}

_LOGGER: logging.Logger = logging.getLogger(__name__)


class Tracing(tracing.Tracing):
    """Tracing affordance implementation using Fuchsia-Controller."""

    def __init__(
            self, device_name: str,
            fuchsia_controller: fc_transport.FuchsiaController,
            reboot_affordance: affordances_capable.RebootCapableDevice) -> None:
        self._name: str = device_name
        self._fc_transport: fc_transport.FuchsiaController = fuchsia_controller

        self._trace_controller_proxy: Optional[
            f_tracingcontroller.Controller.Client]
        self._trace_socket: Optional[fc.Socket]
        self._session_initialized: bool
        self._tracing_active: bool

        # `_reset_state` needs to be called on initialization, and thereafter on
        # every device bootup.
        self._reset_state()
        reboot_affordance.register_for_on_device_boot(fn=self._reset_state)

    def _reset_state(self) -> None:
        """Resets internal state tracking variables to correspond to an inactive
        state; i.e. tracing uniniailized and not started.
        """
        self._trace_socket = None
        self._trace_controller_proxy = None
        self._session_initialized = False
        self._tracing_active = False

    # List all the public methods in alphabetical order
    def initialize(
            self,
            categories: Optional[List[str]] = None,
            buffer_size: Optional[int] = None) -> None:
        """Initializes a trace session.

        Args:
            categories: list of categories to trace.
            buffer_size: buffer size to use in MB.

        Raises:
            errors.FuchsiaStateError: When trace session is already initialized.
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        if self._session_initialized:
            raise errors.FuchsiaStateError(
                f"Trace session is already initialized on {self._name}. Can be "
                "initialized only once")
        _LOGGER.info("Initializing trace session on '%s'", self._name)

        assert self._trace_controller_proxy is None
        self._trace_controller_proxy = f_tracingcontroller.Controller.Client(
            self._fc_transport.connect_device_proxy(
                _FC_PROXIES["TracingController"]))
        trace_socket_server, trace_socket_client = fc.Socket.create()

        try:
            # 1-way FIDL calls do not return a Coroutine, so async isn't needed
            self._trace_controller_proxy.initialize_tracing(
                config=f_tracingcontroller.TraceConfig(
                    categories=categories,
                    buffer_size_megabytes_hint=buffer_size),
                output=trace_socket_server.take())
        except fc.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "fuchsia.tracing.controller.Initialize FIDL Error") from status
        self._trace_socket = trace_socket_client
        self._session_initialized = True

    def start(self) -> None:
        """Starts tracing.

         Raises:
            errors.FuchsiaStateError: When trace session is not initialized or
                already started.
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        if not self._session_initialized:
            raise errors.FuchsiaStateError(
                "Cannot start: Trace session is not "
                f"initialized on {self._name}")
        if self._tracing_active:
            raise errors.FuchsiaStateError(
                f"Cannot start: Trace already started on {self._name}")
        _LOGGER.info("Starting trace on '%s'", self._name)

        try:
            assert self._trace_controller_proxy is not None
            asyncio.run(
                self._trace_controller_proxy.start_tracing(
                    options=f_tracingcontroller.StartOptions(
                        buffer_disposition=f_tracing.BufferDisposition.
                        CLEAR_ENTIRE)))
        except fc.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "fuchsia.tracing.controller.Start FIDL Error") from status
        self._tracing_active = True

    def stop(self) -> None:
        """Stops the current trace.

         Raises:
            errors.FuchsiaStateError: When trace session is not initialized or
                not started.
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        if not self._session_initialized:
            raise errors.FuchsiaStateError(
                "Cannot stop: Trace session is not "
                f"initialized on {self._name}")
        if not self._tracing_active:
            raise errors.FuchsiaStateError(
                f"Cannot stop: Trace not started on {self._name}")
        _LOGGER.info("Stopping trace on '%s'", self._name)

        try:
            assert self._trace_controller_proxy is not None
            asyncio.run(
                self._trace_controller_proxy.stop_tracing(
                    options=f_tracingcontroller.StopOptions(
                        write_results=True)))
        except fc.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "fuchsia.tracing.controller.Stop FIDL Error") from status
        self._tracing_active = False

    def terminate(self) -> None:
        """Terminates the trace session without saving the trace.

         Raises:
            errors.FuchsiaStateError: When trace session is not initialized.
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        self._terminate(download=False)

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
            errors.FuchsiaStateError: When trace session is not initialized or
                already started.
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        trace_buffer: bytes = self._terminate(download=True)

        _LOGGER.info("Collecting trace on '%s'...", self._name)
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
            trace_file_handle.write(trace_buffer)

        _LOGGER.info("Trace downloaded at '%s'", trace_file_path)

        return trace_file_path

    async def _drain_socket_async(self) -> bytes:
        """Drains all of the bytes from the trace socket, until it closes.

        Returns:
            Bytes read from the socket.

        Raises:
            errors.FuchsiaControllerError: When reading from the socket failed.
        """
        assert self._trace_socket is not None

        socket_bytes = bytes()
        while True:
            try:
                socket_bytes = socket_bytes + self._trace_socket.read()
            except fc.ZxStatus as status:
                # ZX_ERR_PEER_CLOSED is expected when the trace_manager is done
                # writing to the socket.
                # ZX_ERR_SHOULD_WAIT is expected when the socket has more data
                # to read.
                zx_status: Optional[int] = \
                    status.args[0] if len(status.args) > 0 else None
                if zx_status == fc.ZxStatus.ZX_ERR_PEER_CLOSED:
                    break
                elif zx_status == fc.ZxStatus.ZX_ERR_SHOULD_WAIT:
                    continue
                else:
                    raise errors.FuchsiaControllerError(
                        "Error reading fuchsia.tracing.controller socket"
                    ) from status
        return socket_bytes

    async def _terminate_and_drain_async(self, download: bool) -> bytes:
        """Concurrently terminates the trace session and (optionally) reads the
        trace data from the trace socket.

        Args:
            download: True if the method should drain the socket, False to skip
                downloading data from the socket.

        Returns:
            Bytes read from the socket.

        Raises:
            ExceptionGroup: If any concurrent tasks failed.
        """
        assert self._trace_controller_proxy is not None

        drain_task: Optional[asyncio.Task] = None
        async with asyncio.TaskGroup() as tg:
            if download:
                drain_task = tg.create_task(self._drain_socket_async())
            tg.create_task(
                self._trace_controller_proxy.terminate_tracing(
                    options=f_tracingcontroller.TerminateOptions(
                        write_results=download)))

        if drain_task is not None:
            return drain_task.result()
        return bytes()

    def _terminate(self, download: bool) -> bytes:
        """Terminates the trace session and optionally reads the trace data from
        the trace socket.

        Args:
            download: True if the method should drain the socket, False to skip
                downloading data from the socket.

        Returns:
            Bytes read from the socket.

        Raises:
            errors.FuchsiaControllerError: When reading from the socket failed.
        """
        if not self._session_initialized:
            raise errors.FuchsiaStateError(
                "Cannot terminate: Trace session is "
                f"not initialized on {self._name}")
        _LOGGER.info("Terminating trace session on '%s'", self._name)

        try:
            socket_bytes = asyncio.run(
                self._terminate_and_drain_async(download=download))
        except ExceptionGroup as grp:
            raise errors.FuchsiaControllerError(
                "fuchsia.tracing.controller.Terminate FIDL Error") from grp
        finally:
            self._reset_state()

        return socket_bytes
