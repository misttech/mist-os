#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Common utils used across HoneyDew."""
import logging
import time
from typing import Callable

from honeydew import errors

_LOGGER: logging.Logger = logging.getLogger(__name__)


def wait_for_state(
        state_fn: Callable[[], bool], expected_state: bool,
        timeout: float) -> None:
    """Wait for specified time for state_fn to return expected_state.

    Args:
        state_fn: function to call for getting current state.
        expected_state: expected state to wait for.
        timeout: How long in sec to wait.

    Raises:
        errors.HoneyDewTimeoutError: If state_fn does not return the
            expected_state with in specified timeout.
    """
    _LOGGER.info(
        "Waiting for %s sec for %s to return %s...", timeout,
        state_fn.__qualname__, expected_state)

    start_time: float = time.time()
    end_time: float = start_time + timeout
    while time.time() < end_time:
        _LOGGER.debug("calling %s", state_fn.__qualname__)
        current_state = state_fn()
        _LOGGER.debug("%s returned %s", state_fn.__qualname__, current_state)
        if current_state == expected_state:
            return
        time.sleep(.5)
    else:
        raise errors.HoneyDewTimeoutError(
            f"{state_fn.__qualname__} didn't return {expected_state} in " \
            f"{timeout} sec")
