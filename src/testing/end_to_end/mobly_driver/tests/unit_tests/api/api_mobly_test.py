#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for Mobly driver's api_mobly.py."""

import unittest
import json
from typing import Any

from tempfile import NamedTemporaryFile
from mobly import config_parser, keys
from parameterized import parameterized, param

from mobly_driver.api import api_mobly


class ApiMoblyTest(unittest.TestCase):
    """API Mobly tests"""

    def test_get_latest_test_output_dir_symlink_path_success(self) -> None:
        """Test case to ensure test output symlink is returned"""
        api_mobly.get_latest_test_output_dir_symlink_path("output_path", "tb")

    @parameterized.expand(  # type: ignore[misc]
        [
            ("Output path is empty", "", "tb"),
            ("Testbed name is empty", "ouput_path", ""),
        ]
    )
    def test_get_latest_test_output_dir_symlink_path_raises_exception(
        self, unused_name: str, output_path: str, tb_name: str
    ) -> None:
        """Test case to ensure exception is raised on test output path errors"""
        with self.assertRaises(api_mobly.ApiException):
            api_mobly.get_latest_test_output_dir_symlink_path(
                output_path, tb_name
            )

    def test_get_result_path_success(self) -> None:
        """Test case to ensure test result symlink is returned"""
        api_mobly.get_result_path("output_path", "tb")

    @parameterized.expand(  # type: ignore[misc]
        [
            ("Output path is empty", "", "tb"),
            ("Testbed name is empty", "ouput_path", ""),
        ]
    )
    def test_get_get_result_path_raises_exception(
        self, unused_name: str, output_path: str, tb_name: str
    ) -> None:
        """Test case to ensure exception is raised on test result path errors"""
        with self.assertRaises(api_mobly.ApiException):
            api_mobly.get_result_path(output_path, tb_name)

    @parameterized.expand(  # type: ignore[misc]
        [
            param(
                "success_empty_controllers_empty_params",
                override_args={
                    "mobly_controllers": [],
                    "test_params_dict": {},
                },
                expected_config_obj={
                    "MoblyParams": {"LogPath": "output_path"},
                    "TestBeds": [
                        {"Controllers": {}, "Name": "tb_name", "TestParams": {}}
                    ],
                },
            ),
            param(
                "success_valid_controllers_valid_params",
                override_args={
                    "mobly_controllers": [
                        {"type": "FuchsiaDevice", "nodename": "fuchsia_abcd"}
                    ],
                    "test_params_dict": {"param": "value"},
                },
                expected_config_obj={
                    "MoblyParams": {"LogPath": "output_path"},
                    "TestBeds": [
                        {
                            "Controllers": {
                                "FuchsiaDevice": [
                                    {
                                        "ffx_path": "ffx_path",
                                        "nodename": "fuchsia_abcd",
                                        "transport": "transport",
                                    }
                                ]
                            },
                            "Name": "tb_name",
                            "TestParams": {"param": "value"},
                        }
                    ],
                },
            ),
            param(
                "success_multiple_controllers_same_type",
                override_args={
                    "mobly_controllers": [
                        {"type": "FuchsiaDevice", "nodename": "fuchsia_abcd"},
                        {"type": "FuchsiaDevice", "nodename": "fuchsia_bcde"},
                    ]
                },
                expected_config_obj={
                    "MoblyParams": {"LogPath": "output_path"},
                    "TestBeds": [
                        {
                            "Controllers": {
                                "FuchsiaDevice": [
                                    {
                                        "ffx_path": "ffx_path",
                                        "nodename": "fuchsia_abcd",
                                        "transport": "transport",
                                    },
                                    {
                                        "ffx_path": "ffx_path",
                                        "nodename": "fuchsia_bcde",
                                        "transport": "transport",
                                    },
                                ]
                            },
                            "Name": "tb_name",
                            "TestParams": {},
                        }
                    ],
                },
            ),
            param(
                "success_multiple_controllers_different_types",
                override_args={
                    "mobly_controllers": [
                        {"type": "FuchsiaDevice", "nodename": "fuchsia_abcd"},
                        {
                            "type": "AccessPoint",
                            "ip": "192.168.42.11",
                            "user": "root",
                            "ssh_key": "some/key/path",
                        },
                    ],
                    "ssh_path": "some/ssh_binary/path",
                },
                expected_config_obj={
                    "MoblyParams": {"LogPath": "output_path"},
                    "TestBeds": [
                        {
                            "Controllers": {
                                "AccessPoint": [
                                    {
                                        "ssh_config": {
                                            "ssh_binary_path": "some/ssh_binary/path",
                                            "host": "192.168.42.11",
                                            "user": "root",
                                            "identity_file": "some/key/path",
                                        },
                                    },
                                ],
                                "FuchsiaDevice": [
                                    {
                                        "ffx_path": "ffx_path",
                                        "nodename": "fuchsia_abcd",
                                        "transport": "transport",
                                    }
                                ],
                            },
                            "Name": "tb_name",
                            "TestParams": {},
                        }
                    ],
                },
            ),
            param(
                "success_controller_with_translation_map",
                override_args={
                    "mobly_controllers": [
                        {
                            "type": "FuchsiaDevice",
                            "nodename": "fuchsia_abcd",
                            "ssh_key": "key",
                        },
                    ],
                    "botanist_honeydew_map": {
                        "nodename": "name",
                        "ssh_key": "ssh_private_key",
                    },
                },
                expected_config_obj={
                    "MoblyParams": {"LogPath": "output_path"},
                    "TestBeds": [
                        {
                            "Controllers": {
                                "FuchsiaDevice": [
                                    {
                                        "ffx_path": "ffx_path",
                                        "name": "fuchsia_abcd",
                                        "ssh_private_key": "key",
                                        "transport": "transport",
                                    }
                                ]
                            },
                            "Name": "tb_name",
                            "TestParams": {},
                        }
                    ],
                },
            ),
            param(
                "success_ffx_subtools_search_path",
                override_args={
                    "mobly_controllers": [
                        {"type": "FuchsiaDevice", "nodename": "fuchsia_abcd"},
                    ],
                    "ffx_subtools_search_path": "some/subtools/search/path",
                },
                expected_config_obj={
                    "MoblyParams": {"LogPath": "output_path"},
                    "TestBeds": [
                        {
                            "Controllers": {
                                "FuchsiaDevice": [
                                    {
                                        "ffx_path": "ffx_path",
                                        "ffx_subtools_search_path": "some/subtools/search/path",
                                        "nodename": "fuchsia_abcd",
                                        "transport": "transport",
                                    }
                                ]
                            },
                            "Name": "tb_name",
                            "TestParams": {},
                        }
                    ],
                },
            ),
            param(
                "success_access_point_ssh_config",
                override_args={
                    "mobly_controllers": [
                        {
                            "type": "AccessPoint",
                            "ip": "192.168.42.11",
                            "user": "root",
                            "ssh_key": "some/key/path",
                        },
                    ],
                    "ssh_path": "some/ssh_binary/path",
                },
                expected_config_obj={
                    "MoblyParams": {"LogPath": "output_path"},
                    "TestBeds": [
                        {
                            "Controllers": {
                                "AccessPoint": [
                                    {
                                        "ssh_config": {
                                            "ssh_binary_path": "some/ssh_binary/path",
                                            "host": "192.168.42.11",
                                            "user": "root",
                                            "identity_file": "some/key/path",
                                        },
                                    },
                                ],
                            },
                            "Name": "tb_name",
                            "TestParams": {},
                        }
                    ],
                },
            ),
        ]
    )
    def test_new_testbed_config(
        self,
        unused_name: str,
        override_args: dict[str, Any],
        expected_config_obj: dict[str, Any],
    ) -> None:
        """Test case for new testbed config generation"""
        config_obj = api_mobly.new_testbed_config(
            testbed_name=override_args.get("testbed_name", "tb_name"),
            output_path=override_args.get("output_path", "output_path"),
            ffx_path=override_args.get("ffx_path", "ffx_path"),
            transport=override_args.get("transport", "transport"),
            mobly_controllers=override_args.get("mobly_controllers", []),
            test_params_dict=override_args.get("test_params_dict", {}),
            botanist_honeydew_map=override_args.get(
                "botanist_honeydew_map", {}
            ),
            ffx_subtools_search_path=override_args.get(
                "ffx_subtools_search_path", None
            ),
            ssh_path=override_args.get("ssh_path", None),
        )
        self.assertEqual(config_obj, expected_config_obj)

        with NamedTemporaryFile(mode="w") as config_fh:
            config_fh.write(json.dumps(config_obj))
            config_fh.flush()
            # Assert that no exceptions are raised.
            config_parser.load_test_config_file(config_fh.name)

    @parameterized.expand(  # type: ignore[misc]
        [
            ("success_empty_config", {}, {}),
            (
                "success_empty_controllers",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {},
                        },
                    ],
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {},
                        },
                    ]
                },
            ),
            (
                "success_valid_controllers",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [{"nodename": "fuchsia_abcd"}]
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "transport": "fuchsia-controller",
                                    }
                                ]
                            },
                        }
                    ]
                },
            ),
            (
                "success_existing_value_is_overriden",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "transport": "foobar",
                                    }
                                ]
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "transport": "fuchsia-controller",
                                    }
                                ]
                            },
                        }
                    ]
                },
            ),
            (
                "success_multiple_controllers_same_type",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                    },
                                    {"nodename": "fuchsia_bcde"},
                                ]
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "transport": "fuchsia-controller",
                                    },
                                    {
                                        "nodename": "fuchsia_bcde",
                                        "transport": "fuchsia-controller",
                                    },
                                ]
                            },
                        }
                    ]
                },
            ),
            (
                "success_multiple_controllers_different_types",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [{"nodename": "fuchsia_abcd"}],
                                "AccessPoint": [{"ip_addr": "1.1.1.1"}],
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "transport": "fuchsia-controller",
                                    }
                                ],
                                "AccessPoint": [{"ip_addr": "1.1.1.1"}],
                            },
                        }
                    ]
                },
            ),
        ]
    )
    def test_set_transport(
        self,
        unused_name: str,
        config_obj: dict[str, Any],
        transformed_config_obj: dict[str, Any],
    ) -> None:
        """Test case for mutating transports in config"""
        new_config_obj = config_obj.copy()
        api_mobly.set_transport(new_config_obj, "fuchsia-controller")
        self.assertDictEqual(new_config_obj, transformed_config_obj)

    @parameterized.expand(  # type: ignore[misc]
        [
            ("success_empty_config", {}, {}),
            (
                "success_empty_controllers",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {},
                        },
                    ],
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {},
                        },
                    ]
                },
            ),
            (
                "success_valid_controllers",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [{"nodename": "fuchsia_abcd"}]
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_path": "ffx/path",
                                    }
                                ]
                            },
                        }
                    ]
                },
            ),
            (
                "success_existing_value_is_overriden",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_path": "foobar",
                                    }
                                ]
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_path": "ffx/path",
                                    }
                                ]
                            },
                        }
                    ]
                },
            ),
            (
                "success_multiple_controllers_same_type",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {"nodename": "fuchsia_abcd"},
                                    {"nodename": "fuchsia_bcde"},
                                ]
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_path": "ffx/path",
                                    },
                                    {
                                        "nodename": "fuchsia_bcde",
                                        "ffx_path": "ffx/path",
                                    },
                                ]
                            },
                        }
                    ]
                },
            ),
            (
                "success_multiple_controllers_different_types",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [{"nodename": "fuchsia_abcd"}],
                                "AccessPoint": [{"ip_addr": "1.1.1.1"}],
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_path": "ffx/path",
                                    }
                                ],
                                "AccessPoint": [{"ip_addr": "1.1.1.1"}],
                            },
                        }
                    ]
                },
            ),
        ]
    )
    def test_set_ffx_path(
        self,
        unused_name: str,
        config_obj: dict[str, Any],
        transformed_config_obj: dict[str, Any],
    ) -> None:
        """Test case for mutating transports in config"""
        new_config_obj = config_obj.copy()
        api_mobly.set_ffx_path(new_config_obj, "ffx/path")
        self.assertDictEqual(new_config_obj, transformed_config_obj)

    @parameterized.expand(  # type: ignore[misc]
        [
            ("success_empty_config", {}, {}),
            (
                "success_empty_controllers",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {},
                        },
                    ],
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {},
                        },
                    ]
                },
            ),
            (
                "success_valid_controllers",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [{"nodename": "fuchsia_abcd"}]
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_subtools_search_path": "path/to/host/tools",
                                    }
                                ]
                            },
                        }
                    ]
                },
            ),
            (
                "success_existing_value_is_overriden",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_subtools_search_path": "old_value",
                                    }
                                ]
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_subtools_search_path": "path/to/host/tools",
                                    }
                                ]
                            },
                        }
                    ]
                },
            ),
            (
                "success_multiple_controllers_same_type",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {"nodename": "fuchsia_abcd"},
                                    {"nodename": "fuchsia_bcde"},
                                ]
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_subtools_search_path": "path/to/host/tools",
                                    },
                                    {
                                        "nodename": "fuchsia_bcde",
                                        "ffx_subtools_search_path": "path/to/host/tools",
                                    },
                                ]
                            },
                        }
                    ]
                },
            ),
            (
                "success_multiple_controllers_different_types",
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [{"nodename": "fuchsia_abcd"}],
                                "AccessPoint": [{"ip_addr": "1.1.1.1"}],
                            },
                        }
                    ]
                },
                {
                    keys.Config.key_testbed.value: [
                        {
                            keys.Config.key_testbed_name.value: "testbed",
                            keys.Config.key_testbed_controllers.value: {
                                "FuchsiaDevice": [
                                    {
                                        "nodename": "fuchsia_abcd",
                                        "ffx_subtools_search_path": "path/to/host/tools",
                                    }
                                ],
                                "AccessPoint": [{"ip_addr": "1.1.1.1"}],
                            },
                        }
                    ]
                },
            ),
        ]
    )
    def test_set_ffx_subtools_search_path(
        self,
        unused_name: str,
        config_obj: dict[str, Any],
        transformed_config_obj: dict[str, Any],
    ) -> None:
        """Test case for mutating transports in config"""
        new_config_obj = config_obj.copy()
        api_mobly.set_ffx_subtools_search_path(
            new_config_obj, "path/to/host/tools"
        )
        self.assertDictEqual(new_config_obj, transformed_config_obj)

    @parameterized.expand(  # type: ignore[misc]
        [
            # (name, config_dict, params_dict, expected_config_dict)
            (
                "Single-testbed config with params",
                {"TestBeds": [{"Name": "tb_1"}]},
                {"param_1": "val_1"},
                {
                    "TestBeds": [
                        {"Name": "tb_1", "TestParams": {"param_1": "val_1"}}
                    ]
                },
            ),
            (
                "Multi-testbed config with params",
                {"TestBeds": [{"Name": "tb_1"}, {"Name": "tb_2"}]},
                {"param_1": "val_1"},
                {
                    "TestBeds": [
                        {"Name": "tb_1", "TestParams": {"param_1": "val_1"}},
                        {"Name": "tb_2", "TestParams": {"param_1": "val_1"}},
                    ]
                },
            ),
            (
                "Empty params",
                {"TestBeds": [{"Name": "tb_1"}]},
                {},
                {"TestBeds": [{"Name": "tb_1", "TestParams": {}}]},
            ),
            (
                "None params",
                {"TestBeds": [{"Name": "tb_1"}]},
                None,
                {"TestBeds": [{"Name": "tb_1", "TestParams": None}]},
            ),
        ]
    )
    def test_get_config_with_test_params_success(
        self,
        unused_name: str,
        config_dict: dict[str, Any],
        params_dict: dict[str, Any],
        expected_config_dict: dict[str, Any],
    ) -> None:
        """Test case for testbed config with params"""
        ret = api_mobly.get_config_with_test_params(config_dict, params_dict)
        self.assertDictEqual(ret, expected_config_dict)

    @parameterized.expand(  # type: ignore[misc]
        [
            ("Config is None", None),
            ("Config is empty", {}),
        ]
    )
    def test_get_config_with_test_params_raises_exception(
        self, unused_name: str, config_dict: dict[str, Any]
    ) -> None:
        """Test case for exceptions in testbed config generation"""
        with self.assertRaises(api_mobly.ApiException):
            api_mobly.get_config_with_test_params(config_dict, {})
