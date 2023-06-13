#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utility module for HTTP requests."""

import json
import logging
import time
from typing import Any, Dict, Iterable, Optional, Type
import urllib.request

from honeydew import errors

_LOGGER: logging.Logger = logging.getLogger(__name__)

_TIMEOUTS: Dict[str, float] = {
    "HTTP_RESPONSE": 30,
}

_DEFAULTS: Dict[str, int] = {
    "ATTEMPTS": 3,
    "INTERVAL": 1,
}


def send_http_request(
    url: str,
    data: Optional[Dict[str, Any]] = None,
    headers: Optional[Dict[str, Any]] = None,
    timeout: float = _TIMEOUTS["HTTP_RESPONSE"],
    attempts: int = _DEFAULTS["ATTEMPTS"],
    interval: int = _DEFAULTS["INTERVAL"],
    exceptions_to_skip: Optional[Iterable[Type[Exception]]] = None
) -> Dict[str, Any]:
    """Send HTTP request and returns the string response.

    This method encodes data dict arg into utf-8 bytes and sets "Content-Type"
    in headers dict arg to "application/json; charset=utf-8".

    Args:
        url: URL to which HTTP request need to be sent.
        data: data that needs to be set in HTTP request.
        headers: headers that need to be included while sending HTTP request.
        timeout: how long in sec to wait for HTTP connection attempt.
        attempts: number of attempts to try in case of a failure.
        interval: wait time in sec before each retry in case of a failure.
        exceptions_to_skip: Any non fatal exceptions for which retry will not be
            attempted.
    Returns:
        Returns the HTTP response received after converting into a dict.

    Raises:
        errors.HttpRequestError: In case of failures.
    """
    if exceptions_to_skip is None:
        exceptions_to_skip = []

    if data is None:
        data = {}
    data_bytes: bytes = json.dumps(data).encode("utf-8")

    if headers is None:
        headers = {}
    headers["Content-Type"] = "application/json; charset=utf-8"
    headers["Content-Length"] = len(data_bytes)

    for attempt in range(1, attempts + 1):
        # if this is not first attempt wait for sometime before next retry.
        if attempt > 1:
            time.sleep(interval)

        try:
            _LOGGER.debug(
                "Sending HTTP request to url=%s with data=%s and headers=%s",
                url, data, headers)
            req = urllib.request.Request(url, data=data_bytes, headers=headers)
            with urllib.request.urlopen(req, timeout=timeout) as response:
                response_body: str = response.read().decode("utf-8")
            _LOGGER.debug(
                "HTTP response received from url=%s is '%s'", url,
                response_body)
            return json.loads(response_body)
        except Exception as err:  # pylint: disable=broad-except
            for exception_to_skip in exceptions_to_skip:
                if isinstance(err, exception_to_skip):
                    return {}

            err_msg: str = f"Send the HTTP request to url={url} with " \
              f"data={data} and headers={headers} failed with error: '{err}'"

            if attempt < attempts:
                _LOGGER.warning(
                    "%s on iteration %s/%s", err_msg, attempt, attempts)
                continue
            raise errors.HttpRequestError(err_msg) from err
    raise errors.HttpRequestError(
        f"Failed to send the HTTP request to url={url} with data={data} and "\
        f"headers={headers}.")
