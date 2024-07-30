# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Common utils used across Honeydew."""
import logging
import time
from collections.abc import Callable

from honeydew import errors
from honeydew.utils import decorators

_LOGGER: logging.Logger = logging.getLogger(__name__)


def _retry_condition(end_time: float | None = None) -> bool:
    if end_time is None:
        return True

    return time.time() < end_time


@decorators.liveness_check
def wait_for_state(
    state_fn: Callable[[], bool],
    expected_state: bool,
    timeout: float | None = None,
    wait_time: float = 1,
) -> None:
    """Wait for specified time for state_fn to return expected_state.

    Args:
        state_fn: function to call for getting current state.
        expected_state: expected state to wait for.
        timeout: How long in sec to wait. By default, no timeout is set.
        wait_time: How long in sec to wait between the retries.

    Raises:
        errors.HoneydewTimeoutError: If state_fn does not return the
            expected_state with in specified timeout.
    """
    message: str = (
        f"Waiting for {state_fn.__qualname__} to return {expected_state}..."
    )
    end_time: float | None = None

    if timeout:
        start_time: float = time.time()
        end_time = start_time + timeout

        message = (
            f"Waiting for {timeout} sec for {state_fn.__qualname__} "
            f"to return {expected_state}..."
        )

    _LOGGER.info(message)

    while _retry_condition(end_time):
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
        time.sleep(wait_time)
    else:
        message = f"{state_fn.__qualname__} didn't return {expected_state}"
        if timeout:
            message += f" in {timeout} sec"
        raise errors.HoneydewTimeoutError(message)


@decorators.liveness_check
def retry(
    fn: Callable[[], object],
    timeout: float | None = None,
    wait_time: int = 1,
) -> object:
    """Wait for specified time for fn to succeed.

    Args:
        fn: function to call.
        timeout: How long in sec to retry in case of failure. By default, no
            timeout is set.
        wait_time: How long in sec to wait between the retries.

    Raises:
        errors.HoneydewTimeoutError: If fn does not succeed with in specified
            timeout.
    """
    message: str = (
        f"Run {fn.__qualname__} until it succeeds, with wait time "
        f"of {wait_time}sec between the retries..."
    )
    end_time: float | None = None

    if timeout:
        start_time: float = time.time()
        end_time = start_time + timeout

        message = (
            f"Run {fn.__qualname__} until it succeeds or {timeout}sec timeout "
            f"hit, with wait time of {wait_time}sec between the retries..."
        )

    _LOGGER.info(message)

    while _retry_condition(end_time):
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
