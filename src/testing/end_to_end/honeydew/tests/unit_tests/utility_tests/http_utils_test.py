#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.utils.http_utils.py."""

from http.client import RemoteDisconnected
import json
from typing import Any, Dict
import unittest
from unittest import mock

from honeydew import errors
from honeydew.utils import http_utils
from parameterized import parameterized

# pylint: disable=protected-access
_PARAMS: Dict[str, Any] = {
    "url": "http://12.34.56.78",
    "data": {
        "key1": "value1",
        "key2": "",
        "key3": {
            "k.3.1": "v.3.1"
        },
    },
    "headers":
        {
            "Content-Type": "application/json; charset=utf-8",
            "Content-Length": 58  # len(json.dumps(_PARAMS["data"]))
        },
    "timeout": http_utils._TIMEOUTS["HTTP_RESPONSE"] + 15,
    "attempts": http_utils._DEFAULTS["ATTEMPTS"] + 5,
    "interval": http_utils._DEFAULTS["INTERVAL"] + 5,
    "exceptions_to_skip": []
}

_MOCK_ARGS: Dict[str, Any] = {
    "urlopen_resp": b'{"id": "", "result": "fuchsia-emulator", "error": null}'
}


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class HttpUtilsTests(unittest.TestCase):
    """Unit tests for honeydew.utils.http_utils.py."""

    @parameterized.expand(
        [
            (
                {
                    "label": "no_optional_params",
                    "url": _PARAMS["url"],
                    "optional_params": {},
                    "urlopen_resp": _MOCK_ARGS["urlopen_resp"],
                },),
            (
                {
                    "label": "data_and_headers",
                    "url": _PARAMS["url"],
                    "optional_params":
                        {
                            "data": _PARAMS["data"],
                            "headers": _PARAMS["headers"],
                        },
                    "urlopen_resp": _MOCK_ARGS["urlopen_resp"],
                },),
            (
                {
                    "label": "data_but_no_headers",
                    "url": _PARAMS["url"],
                    "optional_params": {
                        "data": _PARAMS["data"],
                    },
                    "urlopen_resp": _MOCK_ARGS["urlopen_resp"],
                },),
            (
                {
                    "label": "all_optional_params",
                    "url": _PARAMS["url"],
                    "optional_params":
                        {
                            "data": _PARAMS["data"],
                            "headers": _PARAMS["headers"],
                            "timeout": _PARAMS["timeout"],
                            "attempts": _PARAMS["attempts"],
                            "interval": _PARAMS["interval"],
                            "exceptions_to_skip": _PARAMS["exceptions_to_skip"]
                        },
                    "urlopen_resp": _MOCK_ARGS["urlopen_resp"],
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(http_utils.urllib.request, "urlopen", autospec=True)
    def test_send_http_request_success(
            self, parameterized_dict, mock_urlopen) -> None:
        """Test case for http_utils.send_http_request() success case."""

        urlopen_return_value = mock.MagicMock()
        urlopen_return_value.read.return_value = parameterized_dict[
            "urlopen_resp"]
        urlopen_return_value.__enter__.return_value = urlopen_return_value
        mock_urlopen.return_value = urlopen_return_value

        result: Dict[str, Any] = http_utils.send_http_request(
            url=parameterized_dict["url"],
            **parameterized_dict["optional_params"])

        expected_output: Dict[str, Any] = json.loads(
            parameterized_dict["urlopen_resp"].decode("utf-8"))

        self.assertEqual(result, expected_output)

        mock_urlopen.assert_called_once()

    @mock.patch.object(
        http_utils.urllib.request,
        "urlopen",
        side_effect=RemoteDisconnected,
        autospec=True)
    def test_send_http_request_with_exceptions_to_skip(
            self, mock_urlopen) -> None:
        """Testcase to make sure http_utils.send_http_request() do not
        fail when it receives an exception that is part of exceptions_to_skip
        input arg"""
        response: Dict[str, Any] = http_utils.send_http_request(
            url=_PARAMS["url"], exceptions_to_skip=[RemoteDisconnected])
        self.assertEqual(response, {})
        mock_urlopen.assert_called_once()

    @mock.patch.object(http_utils.time, "sleep", autospec=True)
    @mock.patch.object(
        http_utils.urllib.request,
        "urlopen",
        side_effect=RuntimeError("some run time error"),
        autospec=True)
    def test_send_http_request_fail_because_of_exception(
            self, mock_urlopen, mock_sleep) -> None:
        """Testcase for http_utils.send_http_request() failure case because of
        an exception."""
        with self.assertRaises(errors.HttpRequestError):
            http_utils.send_http_request(
                url=_PARAMS["url"],
                interval=_PARAMS["interval"],
                attempts=_PARAMS["attempts"])

        self.assertEqual(mock_urlopen.call_count, _PARAMS["attempts"])
        self.assertEqual(mock_sleep.call_count, _PARAMS["attempts"] - 1)


if __name__ == "__main__":
    unittest.main()
