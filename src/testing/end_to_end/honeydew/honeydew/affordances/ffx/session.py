#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Session affordance implementation using ffx."""

from honeydew.interfaces.affordances import session


class Session(session.Session):
    """Session affordance implementation using ffx."""

    def start(self) -> None:
        """Start session. It is ok to call `ffx session start` even there is a
           session is started.

        Raises:
            honeydew.errors.SessionError: session failed to start.
        """
        raise NotImplementedError

    def add_component(self, url: str) -> None:
        """Instantiates a component by its URL and adds to the session.

        Args:
            url: url of the component

        Raises:
            honeydew.errors.SessionError: Session failed to launch component
                with given url. Session is not started.
        """
        raise NotImplementedError

    def stop(self) -> None:
        """Stop the session

        Raises:
            honeydew.errors.SessionError: Session failed stop to the session.
                Session is not started.
        """
        raise NotImplementedError
