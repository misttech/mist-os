# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Session affordance implementation using ffx."""

from honeydew.affordances.session import errors as session_errors
from honeydew.affordances.session import session
from honeydew.transports.ffx import errors as ffx_errors
from honeydew.transports.ffx import ffx as ffx_transport


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
        except ffx_errors.FfxCommandError as err:
            raise session_errors.SessionError(err)

        self._started = True

    def is_started(self) -> bool:
        """Check if session is started.

        Returns:
            True if session is started.

        Raises:
            SessionError: failed to check the session state.
        """

        self._started = False
        try:
            res = self._ffx.run(["session", "show"])
            lines = res.splitlines()
            for line in lines:
                if "Execution State:  Running" in line:
                    self._started = True
        except ffx_errors.FfxCommandError as err:
            if (
                'No matching component instance found for query "core/session-manager/session:session"'
                in str(err)
            ):
                # session is not running.
                return False

            raise session_errors.SessionError(err)

        return self._started

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
        except ffx_errors.FfxCommandError as err:
            raise session_errors.SessionError(err)

    def restart(self) -> None:
        """Restart session.

        It is ok to call `ffx session restart` regardless of whether a session
        has started or not.

        Raises:
            honeydew.errors.SessionError: session failed to restart.
        """

        if not self._started:
            raise session_errors.SessionError("session is not started.")

        try:
            self._ffx.run(["session", "restart"])
        except ffx_errors.FfxCommandError as err:
            raise session_errors.SessionError(err)

        self._started = True

    def stop(self) -> None:
        """Stop the session.

        Raises:
            SessionError: Session failed stop to the session.
        """

        try:
            self._ffx.run(["session", "stop"])
        except ffx_errors.FfxCommandError as err:
            raise session_errors.SessionError(err)
        self._started = False
