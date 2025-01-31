# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Session affordance implementation using ffx."""

from honeydew import errors
from honeydew.affordances.session import errors as session_errors
from honeydew.affordances.session import session
from honeydew.transports import ffx as ffx_transport


class SessionUsingFfx(session.Session):
    """Session affordance implementation using ffx.

    Args:
        device_name: Device name returned by `ffx target list`.
        ffx: ffx_transport.FFX.
    """

    def __init__(self, device_name: str, ffx: ffx_transport.FFX) -> None:
        self._name: str = device_name
        self._ffx: ffx_transport.FFX = ffx
        self._started = False

    def start(self) -> None:
        """Start session.

        It is ok to call `ffx session start` even there is a session is
        started.

        Raises:
            SessionError: session failed to start.
        """

        try:
            self._ffx.run(["session", "start"])
        except errors.FfxCommandError as err:
            raise session_errors.SessionError(err)

        self._started = True

    def add_component(self, url: str) -> None:
        """Instantiates a component by its URL and adds to the session.

        Args:
            url: url of the component

        Raises:
            SessionError: Session failed to launch component with given url. Session is not started.
        """
        if not self._started:
            raise session_errors.SessionError("session is not started.")

        try:
            self._ffx.run(["session", "add", url])
        except errors.FfxCommandError as err:
            raise session_errors.SessionError(err)

    def stop(self) -> None:
        """Stop the session.

        Raises:
            SessionError: Session failed stop to the session.
        """

        try:
            self._ffx.run(["session", "stop"])
        except errors.FfxCommandError as err:
            raise session_errors.SessionError(err)
        self._started = False
