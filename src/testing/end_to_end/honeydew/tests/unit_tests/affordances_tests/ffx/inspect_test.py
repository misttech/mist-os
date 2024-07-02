#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.ffx.inspect.py."""

import unittest
from collections.abc import Callable
from unittest import mock

from parameterized import param, parameterized

from honeydew import errors
from honeydew.affordances.ffx import inspect as ffx_inspect
from honeydew.transports import ffx as ffx_transport

_INSPECT_SHOW_JSON_TEXT: str = '[{"data_source":"Inspect","metadata":{"component_url":"fuchsia-pkg://fuchsia.com/system-update-checker#meta/system-update-checker.cm","timestamp":64307780890800},"moniker":"core/system-update/system-update-checker","payload":{"root":{"update-manager":{"update-state":"None","version-available":"None"}}},"version":1}]\n'

_MOCK_ARGS: dict[str, str] = {
    "INSPECT_SHOW_JSON_TEXT": _INSPECT_SHOW_JSON_TEXT,
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
    def test_show(
        self,
        label: str,  # pylint: disable=unused-argument
        selectors: list[str],
        monikers: list[str],
        expected_cmd: list[str],
    ) -> None:
        """Test case for Inspect.show()"""
        self.mock_ffx.run.return_value = _MOCK_ARGS["INSPECT_SHOW_JSON_TEXT"]

        self.inspect_obj.show(
            selectors=selectors,
            monikers=monikers,
        )

        self.mock_ffx.run.assert_called_with(
            cmd=expected_cmd,
            log_output=False,
        )

    @parameterized.expand(
        [
            param(
                label="when_ffx_run_fails_with_FfxCommandError",
                side_effect=errors.FfxCommandError("error"),
                expected_error=errors.InspectError,
            ),
            param(
                label="when_ffx_run_fails_with_DeviceNotConnectedError",
                side_effect=errors.DeviceNotConnectedError("error"),
                expected_error=errors.InspectError,
            ),
            param(
                label="when_ffx_run_fails_with_someother_error",
                side_effect=errors.FfxTimeoutError("error"),
                expected_error=errors.FfxTimeoutError,
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_show_exception(
        self,
        label: str,  # pylint: disable=unused-argument,
        side_effect: type[errors.HoneydewError],
        expected_error: type[errors.HoneydewError],
    ) -> None:
        """Test case for Inspect.show() raising InspectError failure."""
        self.mock_ffx.run.side_effect = side_effect

        with self.assertRaises(expected_error):
            self.inspect_obj.show()
