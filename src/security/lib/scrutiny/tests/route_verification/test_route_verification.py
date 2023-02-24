# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os
import pathlib
import subprocess
import unittest


def main():
    parser = argparse.ArgumentParser(
        description=
        "Check that a 'bad' capability route is rejected while a good one is not mentioned."
    )
    parser.add_argument(
        "--ffx-bin",
        type=pathlib.Path,
        required=True,
        help="Path to the ffx binary.")
    parser.add_argument(
        "--fail-protocol",
        type=str,
        required=True,
        help="Protocol whose route failed and must exist in output.")
    parser.add_argument(
        "--fail-moniker",
        type=str,
        required=True,
        help="Moniker whose route failed and must exist in output.")
    parser.add_argument(
        "--success-protocol",
        type=str,
        required=True,
        help="Protocol whose route succeeded and must not exist in output.")
    parser.add_argument(
        "--depfile",
        type=pathlib.Path,
        required=True,
        help="Path to ninja depfile to write.")
    parser.add_argument(
        "--product-bundle",
        type=pathlib.Path,
        required=True,
        help="Path to the product bundle.")
    args = parser.parse_args()

    # Assume we're in the root build dir right now and that is where we'll find ffx env.
    root_build_dir = os.getcwd()
    ffx_env_path = "./.ffx.env"

    # Imitate the configuration in //src/developer/ffx/build/ffx_action.gni.
    base_config = [
        "analytics.disabled=true",
        "sdk.root=" + root_build_dir,
        "sdk.type=in-tree",
        "sdk.module=host_tools.modular",
    ]

    ffx_args = [args.ffx_bin]
    for c in base_config:
        ffx_args += ["--config", c]
    ffx_args += [
        "--env",
        ffx_env_path,
        "scrutiny",
        "verify",
        "routes",
        "--product-bundle",
        args.product_bundle,
    ]

    test = unittest.TestCase()

    output = subprocess.run(ffx_args, capture_output=True)
    test.assertNotEqual(
        output.returncode, 0, "ffx scrutiny verify must have failed")

    stderr = output.stderr.decode('UTF-8')

    test.assertIn(
        args.fail_protocol, stderr,
        "error message must contain protocol whose route failed")

    test.assertIn(
        args.fail_moniker, stderr,
        "error message must contain moniker whose route failed")

    test.assertNotIn(
        args.success_protocol, stderr,
        "error message must not contain protocol whose route succeeded")
