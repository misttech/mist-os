#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Common utils used across Honeydew."""
import logging
import time
from typing import Callable

from honeydew import errors

_LOGGER: logging.Logger = logging.getLogger(__name__)


def wait_for_state(
    state_fn: Callable[[], bool], expected_state: bool, timeout: float
) -> None:
    """Wait for specified time for state_fn to return expected_state.

    Args:
        state_fn: function to call for getting current state.
        expected_state: expected state to wait for.
        timeout: How long in sec to wait.

    Raises:
        errors.HoneydewTimeoutError: If state_fn does not return the
            expected_state with in specified timeout.
    """
    _LOGGER.info(
        "Waiting for %s sec for %s to return %s...",
        timeout,
        state_fn.__qualname__,
        expected_state,
    )

    start_time: float = time.time()
    end_time: float = start_time + timeout
    while time.time() < end_time:
        _LOGGER.debug("calling %s", state_fn.__qualname__)
        try:
            current_state: bool = state_fn()
            _LOGGER.debug(
                "%s returned %s", state_fn.__qualname__, current_state
            )
            if current_state == expected_state:
                return
        except Exception as err:  # pylint: disable=broad-except
            # `state_fn()` raised an exception. Retry again
            _LOGGER.debug(err)
        time.sleep(0.5)
    else:
        raise errors.HoneydewTimeoutError(
            f"{state_fn.__qualname__} didn't return {expected_state} in "
            f"{timeout} sec"
        )


def retry(
    fn: Callable[[], object], timeout: float, wait_time: int = 1
) -> object:
    """Wait for specified time for fn to succeed.

    Args:
        fn: function to call.
        timeout: How long in sec to retry in case of failure.
        wait_time: How long in sec to wait between the retries.

    Raises:
        errors.HoneydewTimeoutError: If fn does not succeed with in specified
            timeout.
    """
    _LOGGER.info(
        "Retry for %s sec with wait time of %s sec between the retries until "
        "%s succeeds...",
        timeout,
        wait_time,
        fn.__qualname__,
    )

    start_time: float = time.time()
    end_time: float = start_time + timeout
    while time.time() < end_time:
        _LOGGER.debug("calling %s", fn.__qualname__)
        try:
            ret_value: object = fn()
            _LOGGER.debug("%s returned %s", fn.__qualname__, ret_value)
            _LOGGER.info("Successfully finished %s.", fn.__qualname__)
            return ret_value
        except Exception as err:  # pylint: disable=broad-except
            # `fn()` raised an exception. Retry again
            _LOGGER.warning(
                "%s failed with error: %s. Retry again in %s sec",
                fn.__qualname__,
                err,
                wait_time,
            )
        time.sleep(wait_time)
    else:
        raise errors.HoneydewTimeoutError(
            f"{fn.__qualname__} didn't succeed in {timeout} sec"
        )
