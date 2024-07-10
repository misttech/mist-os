#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.ffx.inspect.py."""

import unittest
from collections.abc import Callable
from unittest import mock

import fuchsia_inspect
from parameterized import param, parameterized

from honeydew import errors
from honeydew.affordances.ffx import inspect as ffx_inspect
from honeydew.transports import ffx as ffx_transport

_INSPECT_DATA_JSON_TEXT = """
[
  {
    "data_source": "Inspect",
    "metadata": {
        "component_url": "foo",
        "timestamp": 181016000000000,
        "file_name": "foo.txt"
    },
    "moniker": "core/example",
    "payload": {
      "root": {
        "value": 100
      }
    },
    "version": 1
  },
  {
    "data_source": "Inspect",
    "metadata": {
        "component_url": "foo2",
        "timestamp": 181016000000000
    },
    "moniker": "core/example",
    "payload": {
      "root": {
        "value": 100
      }
    },
    "version": 1
  },
  {
    "data_source": "Inspect",
    "metadata": {
        "component_url": "foo2",
        "timestamp": 181016000000000,
        "errors": [
          {
            "message": "Unknown failure"
          }
        ]
    },
    "moniker": "core/example",
    "payload": null,
    "version": 1
  }
]
"""

_INSPECT_DATA_BAD_VERSION = """
{
    "data_source": "Inspect",
    "metadata": {
        "component_url": "foo",
        "timestamp": 181016000000000,
        "file_name": "foo.txt"
    },
    "moniker": "core/example",
    "payload": {
      "root": {
        "value": 100
      }
    },
    "version": 2
  }
"""


_MOCK_ARGS: dict[str, str] = {
    "INSPECT_DATA_JSON_TEXT": _INSPECT_DATA_JSON_TEXT,
    "INSPECT_DATA_BAD_VERSION": _INSPECT_DATA_BAD_VERSION,
}


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_obj: param
) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__
    test_label: str = parameterized.to_safe_name(param_obj.kwargs["label"])
    return f"{test_func_name}_{test_label}"


class InspectFfxTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.ffx.inspect.py."""

    def setUp(self) -> None:
        super().setUp()
        self.mock_ffx = mock.MagicMock(spec=ffx_transport.FFX)
        self.inspect_obj = ffx_inspect.Inspect(
            device_name="fuchsia-emulator",
            ffx=self.mock_ffx,
        )

    @parameterized.expand(
        [
            param(
                label="without_selectors_and_monikers",
                selectors=None,
                monikers=None,
                expected_cmd=[
                    "--machine",
                    "json",
                    "inspect",
                    "show",
                ],
            ),
            param(
                label="with_one_selector",
                selectors=["selector1"],
                monikers=None,
                expected_cmd=[
                    "--machine",
                    "json",
                    "inspect",
                    "show",
                    "selector1",
                ],
            ),
            param(
                label="with_two_selectors",
                selectors=["selector1", "selector2"],
                monikers=None,
                expected_cmd=[
                    "--machine",
                    "json",
                    "inspect",
                    "show",
                    "selector1",
                    "selector2",
                ],
            ),
            param(
                label="with_one_moniker",
                selectors=None,
                monikers=["core/coll:bar"],
                expected_cmd=[
                    "--machine",
                    "json",
                    "inspect",
                    "show",
                    r"core/coll\:bar",
                ],
            ),
            param(
                label="with_one_selector_and_one_moniker",
                selectors=["selector1"],
                monikers=["core/coll:bar"],
                expected_cmd=[
                    "--machine",
                    "json",
                    "inspect",
                    "show",
                    "selector1",
                    r"core/coll\:bar",
                ],
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_get_data(
        self,
        label: str,  # pylint: disable=unused-argument
        selectors: list[str],
        monikers: list[str],
        expected_cmd: list[str],
    ) -> None:
        """Test case for Inspect.get_data()"""
        self.mock_ffx.run.return_value = _MOCK_ARGS["INSPECT_DATA_JSON_TEXT"]

        inspect_data_collection: fuchsia_inspect.InspectDataCollection = (
            self.inspect_obj.get_data(
                selectors=selectors,
                monikers=monikers,
            )
        )

        self.assertIsInstance(
            inspect_data_collection, fuchsia_inspect.InspectDataCollection
        )
        for inspect_data in inspect_data_collection.data:
            self.assertIsInstance(inspect_data, fuchsia_inspect.InspectData)

        self.mock_ffx.run.assert_called_with(
            cmd=expected_cmd,
            log_output=False,
            timeout=None,
        )

    @parameterized.expand(
        [
            param(
                label="with_FfxCommandError",
                side_effect=errors.FfxCommandError("error"),
                expected_error=errors.InspectError,
            ),
            param(
                label="with_DeviceNotConnectedError",
                side_effect=errors.DeviceNotConnectedError("error"),
                expected_error=errors.InspectError,
            ),
            param(
                label="with_someother_error",
                side_effect=errors.FfxTimeoutError("error"),
                expected_error=errors.FfxTimeoutError,
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_get_data_exception_when_ffx_run_fails(
        self,
        label: str,  # pylint: disable=unused-argument,
        side_effect: type[errors.HoneydewError],
        expected_error: type[errors.HoneydewError],
    ) -> None:
        """Test case for Inspect.get_data() raising InspectError failure."""
        self.mock_ffx.run.side_effect = side_effect

        with self.assertRaises(expected_error):
            self.inspect_obj.get_data()

    def test_get_data_exception_when_inspect_data_parsing_fails(self) -> None:
        """Test case for Inspect.get_data() raising InspectError failure."""
        self.mock_ffx.run.return_value = _MOCK_ARGS["INSPECT_DATA_BAD_VERSION"]

        with self.assertRaises(errors.InspectError):
            self.inspect_obj.get_data()
