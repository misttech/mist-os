#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Entry point of Mobly Driver which conducts Mobly test execution."""

import argparse
import json
import os
import sys
from typing import Any

import mobly_driver
from mobly_driver import driver_factory

parser = argparse.ArgumentParser()
parser.add_argument(
    "mobly_test_path",
    help="path to the Mobly test archive produced by the GN build system.",
)
parser.add_argument(
    "--config-yaml-path",
    default=None,
    help="path to the Mobly test config YAML file.",
)
parser.add_argument(
    "--params-yaml-path",
    default=None,
    help="path to the Mobly test params YAML file.",
)
parser.add_argument(
    "--honeydew-config-json-path",
    default=None,
    help="path to the Honeydew config json file.",
)
parser.add_argument(
    "--test-timeout-sec",
    default=None,
    help="integer to specify number of seconds before a Mobly test times out.",
)
parser.add_argument("--ffx-path", default=None, help="path to FFX.")
parser.add_argument(
    "--ssh-path",
    default=None,
    help="path to SSH binary used by test connectivity labs access points.",
)
parser.add_argument(
    "--ffx-subtools-path",
    default=None,
    help="path to FFX subtools path.",
)
parser.add_argument(
    "--multi-device",
    action="store_const",
    const=True,
    default=False,
    help="Whether the mobly test requires 2+ Fuchsia devices to run.",
)
parser.add_argument(
    "--hermetic",
    action="store_const",
    const=True,
    default=False,
    help="Whether the mobly test is a self-contained executable.",
)
parser.add_argument(
    "--v",
    action="store_const",
    const=True,
    default=False,
    help="run the mobly test with the --v flag.",
)
parser.add_argument(
    "--test_cases",
    nargs="*",
    default=[],
    type=str,
    help="List of test cases to run.",
)
args = parser.parse_args()


def main() -> None:
    """Executes the Mobly test via Mobly Driver.

    This function determines the appropriate Mobly Driver implementation to use
    based on the execution environment, and uses the Mobly Driver to run the
    underlying Mobly test.
    """
    factory = driver_factory.DriverFactory(
        honeydew_config=generate_honeydew_config(),
        multi_device=args.multi_device,
        config_path=args.config_yaml_path,
        params_path=args.params_yaml_path,
        ssh_path=os.path.abspath(args.ssh_path) if args.ssh_path else None,
    )
    driver = factory.get_driver()

    # Use the same Python runtime for Mobly test execution as the one that's
    # currently running this Mobly driver script.
    mobly_driver.run(
        driver=driver,
        python_path=sys.executable,
        test_path=args.mobly_test_path,
        test_cases=args.test_cases,
        timeout_sec=args.test_timeout_sec,
        verbose=args.v,
        hermetic=args.hermetic,
    )


def generate_honeydew_config() -> dict[str, Any]:
    """Generates and returns the Honeydew config.

    Generated Honeydew config will be in the following format:
    {
        "transports": {
            <transport_name>: {
                <key>: <value>,
                ...
            }
            ...
        }
        "affordances": {
            <affordance_name>: {
                <key>: <value>,
                ...
            }
            ...
        }
    }

    Example:
    {
        "transports": {
            "ffx": {
                "path": "/ffx/path",
                "subtools_search_path": "/subtools/path",
            }
        }
        "affordances": {
            "bluetooth": {
                "implementation": "fuchsia-controller"
            }
            "wlan": {
                "implementation": "sl4f"
            }
        }
    }
    """
    ffx_config: dict[str, Any] = {}
    ffx_config["path"] = os.path.abspath(args.ffx_path)
    if args.ffx_subtools_path:
        ffx_config["subtools_search_path"] = os.path.abspath(
            args.ffx_subtools_path
        )

    honeydew_config: dict[str, Any] = {}
    if args.honeydew_config_json_path:
        with open(args.honeydew_config_json_path, "r") as honeydew_config_file:
            config = json.load(honeydew_config_file)
        honeydew_config.update(config)
    if "transports" not in honeydew_config:
        honeydew_config["transports"] = {}
    if "ffx" not in honeydew_config["transports"]:
        honeydew_config["transports"]["ffx"] = {}
    honeydew_config["transports"]["ffx"].update(ffx_config)

    return honeydew_config
