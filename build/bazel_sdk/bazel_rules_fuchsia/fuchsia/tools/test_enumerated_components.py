#!/usr/bin/env python3
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import re
import subprocess

from fuchsia_task_lib import *


class FuchsiaTaskTestEnumeratedComponents(FuchsiaTask):
    def parse_args(self, parser: ScopedArgumentParser) -> argparse.Namespace:
        """Parses arguments."""

        parser.add_argument(
            "--ffx-package",
            type=parser.path_arg(),
            help="A path to the ffx-package subtool.",
            required=True,
        )
        parser.add_argument(
            "--ffx-test",
            type=parser.path_arg(),
            help="A path to the ffx-test subtool.",
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
            required=True,
        )
        parser.add_argument(
            "--package-archive",
            type=parser.path_arg(),
            help="A path to the package archive (.far) file.",
            required=True,
        )
        parser.add_argument(
            "--target",
            help="Optionally specify the target fuchsia device.",
            required=False,
            scope=ArgumentScope.GLOBAL,
        )
        parser.add_argument(
            "--realm",
            help="Optionally specify the target realm to run this test.",
            required=False,
            scope=ArgumentScope.GLOBAL,
        )
        return parser.parse_args()

    def run(self, parser: ScopedArgumentParser) -> None:
        args = self.parse_args(parser)
        target_args = ["--target", args.target] if args.target else []
        url_template = (
            args.url.replace(
                "{{PACKAGE_NAME}}",
                json.loads(args.package_manifest.read_text())["package"][
                    "name"
                ],
            )
            if args.package_manifest
            else args.url
        )

        try:
            contents = subprocess.check_output(
                [
                    args.ffx_package,
                    *target_args,
                    "package",
                    "archive",
                    "list",
                    args.package_archive,
                ],
                text=True,
            )
            meta_components = re.findall(r"\bmeta/\S+\.cm\b", contents)
        except subprocess.CalledProcessError as e:
            raise TaskExecutionException(
                f"Failed to enumerate components for testing!"
            )

        failing_tests = []
        for meta_component in meta_components:
            url = url_template.replace("{{META_COMPONENT}}", meta_component)
            try:
                subprocess.check_call(
                    [
                        args.ffx_test,
                        *target_args,
                        "test",
                        "run",
                        *(["--realm", args.realm] if args.realm else []),
                        url,
                    ]
                )
            except subprocess.CalledProcessError as e:
                failing_tests.append(url)
                if e.returncode != 1:
                    raise e

        if failing_tests:
            raise TaskExecutionException(
                "Test Failures!\nFailing Tests:\n" + "\n".join(failing_tests),
                is_caught_failure=True,
            )


if __name__ == "__main__":
    FuchsiaTaskTestEnumeratedComponents.main()
