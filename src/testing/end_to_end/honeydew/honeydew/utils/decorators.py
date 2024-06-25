#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utility module that contains some useful decorators."""

import functools
import logging
import multiprocessing
import time
from collections.abc import Callable
from timeit import default_timer as timer
from typing import ParamSpec, TypeVar

P = ParamSpec("P")
R = TypeVar("R")

_LIVENESS_CHECK_SLEEP_TIMER: float = 10.0

_LOGGER: logging.Logger = logging.getLogger(__name__)


# Unit test for this is covered as part of unit_tests/utils_tests/host_shell_test.py
def liveness_check(func: Callable[P, R]) -> Callable[P, R]:
    """Decorator that prints liveness check messages until the function
    call is completed."""

    @functools.wraps(func)
    def wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        operation_name: str = func.__name__
        operation_args: str = f"args={args}, kwargs={kwargs}"
        operation: str = f"{operation_name}({operation_args})"

        proc = multiprocessing.Process(
            target=_liveness_check_logger,
            kwargs={
                "operation": operation,
            },
        )

        _LOGGER.debug(
            "[Liveness Check]: Starting a new process to track the liveness "
            "of '%s'",
            operation,
        )
        proc.start()

        _LOGGER.debug(
            "[Liveness Check]: Running '%s'...",
            operation,
        )
        start: float = timer()
        try:
            result: R = func(*args, **kwargs)
            return result
        finally:
            _LOGGER.debug(
                "[Liveness Check]: Stopping the process that was created to "
                "track the liveness of '%s'",
                operation,
            )
            proc.kill()
            proc.join()

            end: float = timer()
            duration: float = end - start

            message: str = (
                f"[Liveness Check]: '{operation}' has been completed."
            )
            if duration >= _LIVENESS_CHECK_SLEEP_TIMER:
                _LOGGER.info(message)
            else:
                _LOGGER.debug(message)

    return wrapper


def _liveness_check_logger(
    operation: str,
) -> None:
    """Helper method used by `@liveness_check()` decorator to log liveness
    messages onto console.

    Args:
        operation: Name of the operation along with the args wrapped in a str.
    """
    while True:
        time.sleep(_LIVENESS_CHECK_SLEEP_TIMER)
        _LOGGER.info(
            "[Liveness Check]: Still waiting on '%s' operation to finish",
            operation,
        )
