#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import subprocess

from fuchsia.tools.fuchsia_task_lib import *


class FuchsiaTaskRegisterDriver(FuchsiaTask):
    def parse_args(self, parser: ScopedArgumentParser) -> argparse.Namespace:
        """Parses arguments."""

        parser.add_argument(
            "--ffx",
            type=parser.path_arg(),
            help="A path to the ffx tool.",
            required=True,
        )
        parser.add_argument(
            "--url",
            type=str,
            help="The full component url.",
            required=True,
        )
        parser.add_argument(
            "--package-manifest",
            type=parser.path_arg(),
            help="A path to the package manifest json file.",
        )
        parser.add_argument(
            "--disable-url",
            type=str,
            help="The pre-existed component url we want to disable.",
            required=False,
        )
        parser.add_argument(
            "--target",
            help="Optionally specify the target fuchsia device.",
            required=False,
            scope=ArgumentScope.GLOBAL,
        )
        return parser.parse_args()

    def run(self, parser: ScopedArgumentParser) -> None:
        args = self.parse_args(parser)
        ffx = [args.ffx] + (["--target", args.target] if args.target else [])
        package_name = (
            json.loads(args.package_manifest.read_text())["package"]["name"]
            if args.package_manifest
            else "{{PACKAGE_NAME}}"
        )

        subprocess.check_call(
            [
                *ffx,
                "driver",
                "register",
                args.url.replace("{{PACKAGE_NAME}}", package_name),
            ]
        )

        # If disable url is provided, we will disable the pre-existing driver
        # after informing the driver manager about the new driver package.
        # Note: The order cannot be swapped.
        if args.disable_url:
            subprocess.check_call(
                [
                    *ffx,
                    "driver",
                    "disable",
                    args.disable_url.replace("{{PACKAGE_NAME}}", package_name),
                ]
            )


if __name__ == "__main__":
    FuchsiaTaskRegisterDriver.main()
