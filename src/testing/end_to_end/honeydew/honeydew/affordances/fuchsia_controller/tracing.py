# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tracing affordance implementation using Fuchsia-Controller."""

import asyncio
import logging
import os
from datetime import datetime

import fidl.fuchsia_tracing as f_tracing
import fidl.fuchsia_tracing_controller as f_tracingcontroller
import fuchsia_controller_py as fc
from fidl import AsyncSocket

from honeydew import errors
from honeydew.interfaces.affordances import tracing
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import fuchsia_controller as fc_transport
from honeydew.typing import custom_types

_FC_PROXIES: dict[str, custom_types.FidlEndpoint] = {
    "TraceProvisioner": custom_types.FidlEndpoint(
        "/core/trace_manager", "fuchsia.tracing.controller.Provisioner"
    ),
    "TracingController": custom_types.FidlEndpoint(
        "/core/trace_manager", "fuchsia.tracing.controller.Session"
    ),
}

_LOGGER: logging.Logger = logging.getLogger(__name__)


class Tracing(tracing.Tracing):
    """Tracing affordance implementation using Fuchsia-Controller."""

    def __init__(
        self,
        device_name: str,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
    ) -> None:
        self._name: str = device_name
        self._fc_transport: fc_transport.FuchsiaController = fuchsia_controller

        self._trace_controller_proxy: f_tracingcontroller.Session.Client | None

        self._trace_socket: AsyncSocket | None
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

    def is_active(self) -> bool:
        """Checks if there is a currently active trace.

        Returns:
            True if the tracing is currently running, False otherwise.
        """
        return self._tracing_active

    def is_session_initialized(self) -> bool:
        """Checks if the session is initialized or not.

        Returns:
            True if the session is initialized, False otherwise.
        """
        return self._session_initialized

    # List all the public methods
    def initialize(
        self,
        categories: list[str] | None = None,
        buffer_size: int | None = None,
        start_timeout_milliseconds: int | None = None,
    ) -> None:
        """Initializes a trace session.

        Args:
            categories: list of categories to trace.
            buffer_size: buffer size to use in MB.
            start_timeout_milliseconds: milliseconds to wait for trace providers
                to acknowledge that they've started tracing. NB: trace providers
                that don't ACK by this deadline may still emit tracing events
                starting at some later point.

        Raises:
            errors.TracingStateError: When trace session is already initialized.
            errors.TracingError: On FIDL communication failure.
        """
        if self._session_initialized:
            raise errors.TracingStateError(
                f"Trace session is already initialized on {self._name}. Can be "
                "initialized only once"
            )
        _LOGGER.info("Initializing trace session on '%s'", self._name)

        assert self._trace_controller_proxy is None
        trace_provisioner_proxy = f_tracingcontroller.Provisioner.Client(
            self._fc_transport.connect_device_proxy(
                _FC_PROXIES["TraceProvisioner"]
            )
        )
        client, server = fc.Channel.create()

        trace_socket_server, trace_socket_client = fc.Socket.create()

        try:
            # 1-way FIDL calls do not return a Coroutine, so async isn't needed
            trace_provisioner_proxy.initialize_tracing(
                controller=server.take(),
                config=f_tracingcontroller.TraceConfig(
                    categories=categories,
                    buffer_size_megabytes_hint=buffer_size,
                    start_timeout_milliseconds=start_timeout_milliseconds,
                ),
                output=trace_socket_server.take(),
            )
        except fc.ZxStatus as status:
            raise errors.TracingError(
                "fuchsia.tracing.controller.Initialize FIDL Error"
            ) from status
        self._trace_controller_proxy = f_tracingcontroller.Session.Client(
            client
        )
        self._trace_socket = AsyncSocket(trace_socket_client)
        self._session_initialized = True

    def start(self) -> None:
        """Starts tracing.

        Raises:
           errors.TracingStateError: When trace session is not initialized or
               already started.
           errors.TracingError: On FIDL communication failure.
        """
        if not self._session_initialized:
            raise errors.TracingStateError(
                "Cannot start: Trace session is not "
                f"initialized on {self._name}"
            )
        if self._tracing_active:
            raise errors.TracingStateError(
                f"Cannot start: Trace already started on {self._name}"
            )
        _LOGGER.info("Starting trace on '%s'", self._name)

        try:
            assert self._trace_controller_proxy is not None
            asyncio.run(
                self._trace_controller_proxy.start_tracing(
                    buffer_disposition=f_tracing.BufferDisposition.CLEAR_ENTIRE
                )
            )
        except fc.ZxStatus as status:
            raise errors.TracingError(
                "fuchsia.tracing.controller.Start FIDL Error"
            ) from status
        self._tracing_active = True

    def stop(self) -> None:
        """Stops the current trace.

        Raises:
           errors.TracingStateError: When trace session is not initialized or
               not started.
           errors.TracingError: On FIDL communication failure.
        """
        if not self._session_initialized:
            raise errors.TracingStateError(
                "Cannot stop: Trace session is not "
                f"initialized on {self._name}"
            )
        if not self._tracing_active:
            raise errors.TracingStateError(
                f"Cannot stop: Trace not started on {self._name}"
            )
        _LOGGER.info("Stopping trace on '%s'", self._name)

        try:
            assert self._trace_controller_proxy is not None
            stop_tracing_result = asyncio.run(
                self._trace_controller_proxy.stop_tracing(write_results=True)
            )
            if stop_tracing_result.response is not None:
                stop_tracing_response = stop_tracing_result.response
                provider_stats = stop_tracing_response.provider_stats
                for p in provider_stats:
                    if p.records_dropped and p.records_dropped > 0:
                        _LOGGER.warning(
                            "%s records were dropped for %s!",
                            p.records_dropped,
                            p.name,
                        )
        except fc.ZxStatus as status:
            raise errors.TracingError(
                "fuchsia.tracing.controller.Stop FIDL Error"
            ) from status
        self._tracing_active = False

    def terminate(self) -> None:
        """Terminates the trace session without saving the trace."""
        if self._trace_controller_proxy is not None:
            self._trace_controller_proxy.channel.close()
        self._reset_state()

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
                "trace_{device_name}_{'%Y-%m-%d-%I-%M-%S-%p'}" format.

        Returns:
            The path to the trace file.

         Raises:
            errors.TracingStateError: When trace session is not initialized or
                already started.
        """
        if not self._session_initialized:
            raise errors.TracingStateError(
                "Cannot download: Trace session is not "
                f"initialized on {self._name}"
            )
        trace_buffer: bytes = asyncio.run(self._drain_socket())

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

    async def _drain_socket(self) -> bytes:
        """Drains all of the bytes from the trace socket, until it closes.

        Returns:
            Bytes read from the socket.

        Raises:
            errors.TracingError: When reading from the socket failed.
        """
        assert self._trace_socket is not None

        socket_task = asyncio.get_running_loop().create_task(
            self._trace_socket.read_all()
        )
        self.terminate()

        trace_buffer: bytes = await socket_task

        return trace_buffer
