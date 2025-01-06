# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Validate the contents of component manifests."""

import argparse
import subprocess


def get_parser():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--component-manifest-paths",
        help="Paths to component manifest files, in a form of comma separated str",
        required=True,
    )
    parser.add_argument(
        "--package-manifest-path",
        help="Path to package manifest",
        required=True,
    )
    parser.add_argument("--cmc", help="Path to cmc tool", required=True)
    parser.add_argument(
        "--output",
        help="Path to depfile that indicate this action is done",
        required=True,
    )
    return parser


def validate_component_manifest(
    cmc: str, package_manifest: str, component_manifest: str
) -> None:
    # This call will validate component manifest against package manifest. Sample error:
    #
    #   Error: Failed to validate manifest: "bazel-out/aarch64-fastbuild-ST-0244bc8badbe/bin/build/bazel/examples/hello_rust/meta/hello_rust.cm"
    #   program.binary=bin/hello_rust but bin/hello_rust is not provided by deps!
    #
    #   Did you mean bin/hello_rust_bin?
    #
    #   Try any of the following:
    #   bin/hello_rust_bin
    #   lib/ld.so.1
    #   lib/libc++.so.2
    #   lib/libc++abi.so.1
    #   lib/libfdio.so
    #   lib/libunwind.so.1
    #   meta/hello_rust.cm
    #   meta/package

    subprocess.check_call(
        [
            cmc,
            "validate-references",
            "--component-manifest",
            component_manifest,
            "--package-manifest",
            package_manifest,
        ]
    )


def main():
    args = get_parser().parse_args()
    cmc = args.cmc
    package_manifest = args.package_manifest_path
    component_manifests = args.component_manifest_paths.split(",")

    for component_manifest in component_manifests:
        validate_component_manifest(cmc, package_manifest, component_manifest)

    with open(args.output, "w") as f:
        f.write("")


if __name__ == "__main__":
    main()
