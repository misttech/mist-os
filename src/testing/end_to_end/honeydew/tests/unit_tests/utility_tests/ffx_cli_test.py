#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.utils.ffx_cli.py."""

import subprocess
import unittest
from unittest import mock

from honeydew import errors
from honeydew.utils import ffx_cli
from parameterized import parameterized

_FFX_TARGET_SHOW_OUTPUT = \
    '[{"title":"Target","label":"target","description":"",' \
    '"child":[{"title":"Name","label":"name","description":"Target name.",' \
    '"value":"fuchsia-emulator"},{"title":"SSH Address",' \
    '"label":"ssh_address","description":"Interface address",' \
    '"value":"fe80::3804:df7d:daa8:ce6c%qemu:22"}]},{"title":"Build",' \
    '"label":"build","description":"","child":[{"title":"Version",' \
    '"label":"version","description":"Build version.",' \
    '"value":"2023-02-01T17:26:40+00:00"},{"title":"Product",' \
    '"label":"product","description":"Product config.",' \
    '"value":"workstation_eng"},{"title":"Board","label":"board",' \
    '"description":"Board config.","value":"qemu-x64"},{"title":"Commit",' \
    '"label":"commit","description":"Integration Commit Date",' \
    '"value":"2023-02-01T17:26:40+00:00"}]}]'.encode()

_FFX_TARGET_SHOW_JSON = [
    {
        'title':
            'Target',
        'label':
            'target',
        'description':
            '',
        'child':
            [
                {
                    'title': 'Name',
                    'label': 'name',
                    'description': 'Target name.',
                    'value': 'fuchsia-emulator'
                }, {
                    'title': 'SSH Address',
                    'label': 'ssh_address',
                    'description': 'Interface address',
                    'value': 'fe80::3804:df7d:daa8:ce6c%qemu:22'
                }
            ]
    }, {
        'title':
            'Build',
        'label':
            'build',
        'description':
            '',
        'child':
            [
                {
                    'title': 'Version',
                    'label': 'version',
                    'description': 'Build version.',
                    'value': '2023-02-01T17:26:40+00:00'
                }, {
                    'title': 'Product',
                    'label': 'product',
                    'description': 'Product config.',
                    'value': 'workstation_eng'
                }, {
                    'title': 'Board',
                    'label': 'board',
                    'description': 'Board config.',
                    'value': 'qemu-x64'
                }, {
                    'title': 'Commit',
                    'label': 'commit',
                    'description': 'Integration Commit Date',
                    'value': '2023-02-01T17:26:40+00:00'
                }
            ]
    }
]


def _custom_test_name_func(testcase_func, _, param):
    """Custom name function method."""
    test_func_name = testcase_func.__name__

    params_dict = param.args[0]
    test_label = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class FfxCliTests(unittest.TestCase):
    """Unit tests for honeydew.utils.ffx_cli.py."""

    def setUp(self) -> None:
        super().setUp()

        # Start all the mock patches
        self.mock_check_output = mock.patch(
            "honeydew.utils.host_utils.subprocess.check_output",
            return_value=_FFX_TARGET_SHOW_OUTPUT,
            autospec=True).start()

        # Make sure all mock patches are stopped when the test is completed.
        self.addCleanup(mock.patch.stopall)

    def test_ffx_target_show_when_connected(self):
        """Verify ffx_target_show succeeds when target is connected to host."""
        result = ffx_cli.ffx_target_show(target="fuchsia-emulator")
        self.assertEqual(result, _FFX_TARGET_SHOW_JSON)

        self.mock_check_output.assert_called_once_with(
            "ffx -t fuchsia-emulator --timeout 10 target show --json",
            shell=True,
            stderr=subprocess.STDOUT)

    def test_ffx_target_show_when_not_connected(self):
        """Verify ffx_target_show raises an exception when target is not
        connected to host."""
        self.mock_check_output.side_effect = subprocess.CalledProcessError(
            120, "some_cmd")

        with self.assertRaises(errors.FfxCommandError):
            ffx_cli.ffx_target_show(target="fuchsia-d88c-799b-0e3a")

        self.mock_check_output.assert_called_once_with(
            "ffx -t fuchsia-d88c-799b-0e3a --timeout 10 target show --json",
            shell=True,
            stderr=subprocess.STDOUT)

    def test_get_target_address(self):
        """Verify get_target_address returns ip address of fuchsia device."""
        result = ffx_cli.get_target_address(target="fuchsia-emulator")
        expected = _FFX_TARGET_SHOW_JSON[0]['child'][1]['value'][:-3]

        self.assertEqual(result, expected)

        self.mock_check_output.assert_called_once_with(
            "ffx -t fuchsia-emulator --timeout 10 target show --json",
            shell=True,
            stderr=subprocess.STDOUT)

    @parameterized.expand(
        [
            ({
                "label": "empty_output",
                "side_effect": b'[]'
            },),
            (
                {
                    "label":
                        "CalledProcessError",
                    "side_effect":
                        subprocess.CalledProcessError(
                            returncode=1,
                            cmd="ffx -t fuchsia-emulator target show")
                },),
        ],
        name_func=_custom_test_name_func)
    def test_get_target_address_exception(self, parameterized_dict):
        """Verify get_target_address raise exception in failure cases."""
        self.mock_check_output.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(errors.FfxCommandError):
            ffx_cli.get_target_address(target="fuchsia-emulator")

        self.mock_check_output.assert_called_once_with(
            "ffx -t fuchsia-emulator --timeout 10 target show --json",
            shell=True,
            stderr=subprocess.STDOUT)

    def test_get_target_type(self):
        """Verify get_target_type returns target type of fuchsia device."""
        result = ffx_cli.get_target_type(target="fuchsia-emulator")
        expected = _FFX_TARGET_SHOW_JSON[1]['child'][2]['value']

        self.assertEqual(result, expected)

        self.mock_check_output.assert_called_once_with(
            "ffx -t fuchsia-emulator --timeout 10 target show --json",
            shell=True,
            stderr=subprocess.STDOUT)

    @parameterized.expand(
        [
            ({
                "label": "empty_output",
                "side_effect": b'[]'
            },),
            (
                {
                    "label":
                        "CalledProcessError",
                    "side_effect":
                        subprocess.CalledProcessError(
                            returncode=1,
                            cmd="ffx -t fuchsia-emulator target show")
                },),
        ],
        name_func=_custom_test_name_func)
    def test_get_target_type_exception(self, parameterized_dict):
        """Verify get_target_type raise exception in failure cases."""
        self.mock_check_output.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(errors.FfxCommandError):
            ffx_cli.get_target_type(target="fuchsia-emulator")

        self.mock_check_output.assert_called_once_with(
            "ffx -t fuchsia-emulator --timeout 10 target show --json",
            shell=True,
            stderr=subprocess.STDOUT)


if __name__ == '__main__':
    unittest.main()
