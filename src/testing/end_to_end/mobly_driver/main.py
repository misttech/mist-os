#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Entry point of Mobly Driver which conducts Mobly test execution."""

import argparse
import sys

import driver_factory
import mobly_driver_lib

parser = argparse.ArgumentParser()
parser.add_argument(
    'mobly_test_path',
    help='path to the Mobly test archive produced by the GN build system.')
parser.add_argument(
    '-config_yaml_path',
    default=None,
    help='path to the Mobly test config YAML file.')
parser.add_argument(
    '-params_yaml_path',
    default=None,
    help='path to the Mobly test params YAML file.')
parser.add_argument(
    '-test_timeout_sec',
    default=0,
    help='integer to specify number of seconds before a Mobly test times out.')
parser.add_argument(
    '-test_data_path',
    default=None,
    help='path to directory containing test-time data dependencies.')
parser.add_argument(
    '-transport',
    default=None,
    help='value to use in mobly config for host->device transport type.')
parser.add_argument(
    '-v',
    action='store_const',
    const=True,
    default=False,
    help='run the mobly test with the -v flag.')
args = parser.parse_args()


def main():
    """Executes the Mobly test via Mobly Driver.

    This function determines the appropriate Mobly Driver implementation to use
    based on the execution environment, and uses the Mobly Driver to run the
    underlying Mobly test.
    """
    factory = driver_factory.DriverFactory(
        config_path=args.config_yaml_path,
        params_path=args.params_yaml_path,
    )
    driver = factory.get_driver()

    # Use the same Python runtime for Mobly test execution as the one that's
    # currently running this Mobly driver script.
    mobly_driver_lib.run(
        driver=driver,
        python_path=sys.executable,
        test_path=args.mobly_test_path,
        timeout_sec=args.test_timeout_sec,
        test_data_path=args.test_data_path,
        transport=args.transport,
        verbose=args.v)
