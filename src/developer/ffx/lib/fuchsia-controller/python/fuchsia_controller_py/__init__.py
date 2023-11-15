# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import fuchsia_controller_internal
import tempfile
import shutil
import os
import typing
from fuchsia_controller_internal import ZxStatus


def connect_handle_notifier() -> int:
    return fuchsia_controller_internal.connect_handle_notifier()


def encode_ints(ints) -> bytes:
    """
    Encodes an array-like object of int-like things to a byte buffer.
    """
    return b"".join([x.to_bytes(4, byteorder="little") for x in ints])


class Handle:
    """
    Fuchsia controller FIDL handle. This is used to bootstrap processes for FIDL interactions.
    """

    def __init__(self, handle):
        self._handle = handle

    def as_int(self) -> int:
        """
        Returns the underlying handle as an integer.
        """
        return fuchsia_controller_internal.handle_as_int(self._handle)

    def take(self) -> int:
        """
        Takes the underlying fidl handle, setting it internally to zero (thus invalidating the
        underlying channel). This is used for sending a handle through FIDL function calls.
        """
        return fuchsia_controller_internal.handle_take(self._handle)

    def close(self):
        """
        Releases the underlying handle.
        """
        self._handle = None

    @classmethod
    def create(cls) -> "Handle":
        """
        classmethod for creating a Fuchsia controller handle.

        Returns:
            A Handle object.
        """
        return Handle(fuchsia_controller_internal.handle_create())


class Socket:
    """
    Fuchsia controller Zircon socket. This can be read from and written to.

    Can be constructed from a Handle object, but keep in mind that this will mark
    the caller's handle invalid, leaving this socket to be the only owner of the underlying
    handle.
    """

    def __init__(self, handle):
        if isinstance(handle, Handle):
            handle = handle.take()
        if isinstance(handle, int):
            self._handle = fuchsia_controller_internal.socket_from_int(handle)
        else:
            self._handle = handle

    def write(self, data) -> int:
        """
        Writes data to the socket.

        Args:
            data: The data to write to the socket. This must be a tuple of two elements
            containing bytes and handles.

        Returns:
            The number of bytes written.

        Raises:
            TypeError: If data is not the correct type.
        """
        return fuchsia_controller_internal.socket_write(self._handle, data)

    def read(self) -> bytes:
        """
        Reads data from the socket.
        """
        return fuchsia_controller_internal.socket_read(self._handle)

    def as_int(self) -> int:
        """
        Returns the underlying socket as an integer.
        """
        return fuchsia_controller_internal.socket_as_int(self._handle)

    def take(self) -> int:
        """
        Takes the underlying fidl handle, setting it internally to zero (thus invalidating the
        underlying socket). This is used for sending a handle through FIDL function calls.
        """
        return fuchsia_controller_internal.socket_take(self._handle)

    def close(self):
        """
        Releases the underlying handle.
        """
        self._handle = None

    @classmethod
    def create(cls, options=None) -> tuple["Socket", "Socket"]:
        """
        classmethod for creating a pair of sockets. These are connected bidirectionally.

        Returns:
            A tuple of two Socket objects.
        """
        if options is None:
            options = 0
        sockets = fuchsia_controller_internal.socket_create(options)
        return (Socket(sockets[0]), Socket(sockets[1]))


class IsolateDir:
    """
    Fuchsia controller Isolate Directory. Represents an Isolate Directory path to be used by the fuchsia controller Context object. This object cleans up the Isolate Directory (if it exists) once it goes out of scope.
    """

    def __init__(self, dir: typing.Optional[str] = None) -> None:
        self._handle = fuchsia_controller_internal.isolate_dir_create(dir)

    def directory(self) -> str:
        """
        Returns a string representing the directory to which this IsolateDir points. The IsolateDir will create it upon initialization.
        """
        return fuchsia_controller_internal.isolate_dir_get_path(self._handle)


class Context:
    """
    Fuchsia controller context. This is the necessary object for interacting with a Fuchsia device.
    """

    def __init__(
        self,
        config=None,
        isolate_dir: IsolateDir = IsolateDir(),
        target: typing.Optional[str] = None,
    ) -> None:
        self._handle = fuchsia_controller_internal.context_create(
            config, isolate_dir.directory(), target
        )
        self._directory = isolate_dir

    def connect_daemon_protocol(self, marker: str) -> "Channel":
        """
        Connects to a Fuchsia daemon protocol.

        Args:
            marker: The marker of the protocol to connect to.

        Returns:
            A FIDL client for the protocol.
        """
        return Channel(
            fuchsia_controller_internal.context_connect_daemon_protocol(
                self._handle, marker
            )
        )

    def target_wait(self, timeout: float) -> bool:
        """
        Waits for the target to be ready.

        Args:
            timeout: The timeout in seconds.

        Returns:
            True if the target is ready, False otherwise.
        """
        return fuchsia_controller_internal.context_target_wait(
            self._handle, timeout
        )

    def connect_target_proxy(self) -> "Channel":
        """
        Connects to the target proxy.

        Returns:
            A FIDL client for the target proxy.
        """
        return Channel(
            fuchsia_controller_internal.context_connect_target_proxy(
                self._handle
            )
        )

    def connect_device_proxy(
        self, moniker: str, capability_name: str
    ) -> "Channel":
        """
        Connects to a device proxy.

        Args:
            moniker: The component moniker to connect to
            capability_name: The capability to connect to

        Returns:
            A FIDL client for the device proxy.
        """
        return Channel(
            fuchsia_controller_internal.context_connect_device_proxy(
                self._handle, moniker, capability_name
            )
        )

    def connect_remote_control_proxy(self) -> "Channel":
        """
        Connects to the remote control proxy.

        Returns:
            A FIDL client for the remote control proxy.
        """
        return Channel(
            fuchsia_controller_internal.context_connect_remote_control_proxy(
                self._handle
            )
        )

    def close(self):
        """
        Releases the underlying handle.
        """
        self._handle = None


class Channel:
    """
    Fuchsia controller FIDL channel. This can be read from and written to.

    Can be constructed from a Handle object, but keep in mind that this will mark
    the caller's handle invalid, leaving this channel to be the only owner of the underlying
    handle.
    """

    def __init__(self, handle):
        if isinstance(handle, Handle):
            handle = handle.take()
        if isinstance(handle, int):
            self._handle = fuchsia_controller_internal.channel_from_int(handle)
        else:
            self._handle = handle

    def write(self, data) -> int:
        """
        Writes data to the channel.

        Args:
            data: The data to write to the channel. This must be a tuple of two elements
            containing bytes and handles.

        Returns:
            The number of bytes written.

        Raises:
            TypeError: If data is not the correct type.
        """
        bytes = encode_ints(
            [elem for handle_desc in data[1] for elem in handle_desc]
        )
        return fuchsia_controller_internal.channel_write(
            self._handle, data[0], bytes
        )

    def read(self) -> typing.Tuple[bytes, typing.List[Handle]]:
        """
        Reads data from the channel.
        """
        retval = fuchsia_controller_internal.channel_read(self._handle)
        # Convert internal Handle objects to Python Handle objects
        return (retval[0], list(map(Handle, retval[1])))

    def as_int(self) -> int:
        """
        Returns the underlying channel as an integer.
        """
        return fuchsia_controller_internal.channel_as_int(self._handle)

    def take(self) -> int:
        """
        Takes the underlying fidl handle, setting it internally to zero (thus invalidating the
        underlying channel). This is used for sending a handle through FIDL function calls.
        """
        return fuchsia_controller_internal.channel_take(self._handle)

    def close(self):
        """
        Releases the underlying handle.
        """
        self._handle = None

    @classmethod
    def create(cls) -> tuple["Channel", "Channel"]:
        """
        classmethod for creating a pair of channels. These are connected bidirectionally.

        Returns:
            A tuple of two Channel objects.
        """
        handles = fuchsia_controller_internal.channel_create()
        return (Channel(handles[0]), Channel(handles[1]))
