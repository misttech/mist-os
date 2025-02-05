# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Session affordance."""

import abc


class Session(abc.ABC):
    """Abstract base class for Session affordance."""

    @abc.abstractmethod
    def start(self) -> None:
        """Start session.

        Raises:
            honeydew.errors.SessionError: session failed to start.
        """

    @abc.abstractmethod
    def add_component(self, url: str) -> None:
        """Instantiates a component by its URL and adds to the session.

        Args:
            url: url of the component

        Raises:
            honeydew.errors.SessionError: Session failed to launch component
                with given url. Session is not started.
        """

    @abc.abstractmethod
    def stop(self) -> None:
        """Stop the session

        Raises:
            honeydew.errors.SessionError: Session failed to stop the session.
        """
