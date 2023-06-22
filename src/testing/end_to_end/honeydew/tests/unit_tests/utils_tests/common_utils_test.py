#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.utils.common.py."""

import unittest

from honeydew import errors
from honeydew.utils import common


class CommonUtilsTests(unittest.TestCase):
    """Unit tests for honeydew.utils.common.py."""

    def test_wait_for_state_success(self) -> None:
        """Test case for common.wait_for_state() success case."""
        common.wait_for_state(
            state_fn=lambda: True, expected_state=True, timeout=1)

    def test_wait_for_state_fail(self) -> None:
        """Test case for common.wait_for_state() failure case."""
        with self.assertRaises(errors.HoneyDewTimeoutError):
            common.wait_for_state(
                state_fn=lambda: True, expected_state=False, timeout=1)
