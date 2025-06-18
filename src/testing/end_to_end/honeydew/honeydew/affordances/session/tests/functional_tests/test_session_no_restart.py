# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Session affordance."""

import logging
from typing import Set

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.affordances.session import errors as session_errors
from honeydew.affordances.session import session_using_ffx
from honeydew.fuchsia_device import fuchsia_device

_LOGGER = logging.getLogger(__name__)

_TILE_URL = (
    "fuchsia-pkg://fuchsia.com/flatland-examples#meta/flatland-rainbow.cm"
)


class SessionAffordanceNoRestartTests(fuchsia_base_test.FuchsiaBaseTest):
    """Session affordance tests without restart

    This test suite only contains tests that do not restart the session.
    So we can keep test coverage on platform having flaky issue on session
    restart.
    """

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_add_component(self) -> None:
        """Test case for session.add_component()"""

        self.device.session.ensure_started()
        self.device.session.add_component(_TILE_URL)

    def test_add_component_wrong_url(self) -> None:
        """Test case for session.add_component() with wrong url."""

        self.device.session.ensure_started()

        wrong_url = "INVALID_URL"

        with asserts.assert_raises(session_errors.SessionError):
            self.device.session.add_component(wrong_url)

    def test_add_component_twice(self) -> None:
        """Test case for session.add_component() called twice."""
        self.device.session.ensure_started()
        self.device.session.add_component(_TILE_URL)
        self.device.session.add_component(_TILE_URL)

    def _elements(self) -> Set[str]:
        """Get current components"""
        res = self.device.ffx.run(["component", "list"])
        lines = [
            line
            for line in res.splitlines()
            if line.startswith(session_using_ffx._ELEMENT_PREFIX)
        ]
        return set(lines)

    def test_cleanup(self) -> None:
        """Test case for session.cleanup()."""

        self.device.session.ensure_started()

        before_add = self._elements()
        self.device.session.add_component(_TILE_URL)
        after_add = self._elements()

        added_elements = after_add - before_add
        asserts.assert_equal(len(added_elements), 1)
        added_element = list(added_elements)[0]

        _LOGGER.info(f"added element: {added_element}")

        session = self.device.session
        session._cleanup()

        after_cleanup = self._elements()

        _LOGGER.info(f"after cleanup: {after_cleanup}")

        # The cleanup should remove the newly added component.
        asserts.assert_false(
            added_element in after_cleanup, "The added element is not removed."
        )


if __name__ == "__main__":
    test_runner.main()
