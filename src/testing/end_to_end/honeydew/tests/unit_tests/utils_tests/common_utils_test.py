#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.utils.common.py."""

import unittest
from unittest import mock

from honeydew import errors
from honeydew.utils import common


class CommonUtilsTests(unittest.TestCase):
    """Unit tests for honeydew.utils.common.py."""

    def test_wait_for_state_success(self) -> None:
        """Test case for common.wait_for_state() success case."""
        common.wait_for_state(
            state_fn=lambda: True, expected_state=True, timeout=5
        )

    @mock.patch("time.sleep", autospec=True)
    @mock.patch("time.time", side_effect=[0, 1, 2, 3, 4, 5], autospec=True)
    def test_wait_for_state_fail(self, mock_time, mock_sleep) -> None:
        """Test case for common.wait_for_state() failure case where state_fn
        never returns the expected state."""
        with self.assertRaises(errors.HoneyDewTimeoutError):
            common.wait_for_state(
                state_fn=lambda: True, expected_state=False, timeout=5
            )

        mock_time.assert_called()
        mock_sleep.assert_called()

    @mock.patch("time.sleep", autospec=True)
    @mock.patch("time.time", side_effect=[0, 1, 2, 3, 4, 5], autospec=True)
    def test_wait_for_state_fail_2(self, mock_time, mock_sleep) -> None:
        """Test case for common.wait_for_state() failure case where state_fn
        keeps raising exception."""

        def _state_fn() -> bool:
            raise RuntimeError("Error")

        with self.assertRaises(errors.HoneyDewTimeoutError):
            common.wait_for_state(
                state_fn=_state_fn, expected_state=False, timeout=5
            )

        mock_time.assert_called()
        mock_sleep.assert_called()
