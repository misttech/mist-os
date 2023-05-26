#!/usr/bin/env fuchsia-vendored-python

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
A Script for creating a local version of the merged Bazel SDK.

To release the Bazel SDK we run a recipe in infrastructure that recipe builds
the SDK for all architectures and then merges them into a single directory that
gets released to CIPD. This script recreates what infrastructure is doing so we
can create a merged SDK locally. This script should only be used for debugging
purposes and should not be considered to produce the exact output as that which
is created in the infrastructure recipe.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

REPOSITORY_ROOT = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "..", ".."))
MERGE_SCRIPT = os.path.abspath(
    os.path.join(
        REPOSITORY_ROOT, "scripts", "sdk", "bazel", "merge_bazel_wrapper.py"))
SUPPORTED_ARCHITECTURES = ["x64", "arm64"]


def get_current_build_dir(cwd):
    """Returns the current build dir."""
    return subprocess.check_output(["fx", "get-build-dir"], cwd=cwd).decode()


def build_sdk(build_dir, arch):
    """Builds the SDK in the specified build_dir"""
    set_args = ["fx", "--dir", build_dir, "set", "minimal.{}".format(arch)]
    build_args = [
        "fx", "--dir", build_dir, "build", "generate_fuchsia_sdk_repository"
    ]
    print("INFO: building sdk for {} at {}".format(arch, build_dir))
    subprocess.check_call(set_args)
    subprocess.check_call(build_args)


def copy_sdk(build_dir, dest, arch):
    """Copies the sdk from build_dir to dest"""
    with open(os.path.join(build_dir, "bazel_sdk_info.json"), 'r') as f:
        contents = json.load(f)
        location = os.path.join(build_dir, contents[0]["location"])
        destination_dir = os.path.join(dest, "sdk-{}".format(arch))
        print(
            "INFO: copying contents of {} to {}".format(
                location, destination_dir))
        shutil.copytree(location, destination_dir)


def build_and_copy_all_sdks(dest, skip_build=False):
    """Builds the SDK for all supported architectures and copies them to dest"""
    print("INFO: Building the sdk for all architectures. The contents will")
    print(" be built in out/gen-sdk-<ARCH> and will NOT be cleaned up.")
    for arch in SUPPORTED_ARCHITECTURES:
        build_dir = os.path.join("out", "gen-sdk-{}".format(arch))
        if not skip_build:
            build_sdk(build_dir, arch)
        copy_sdk(build_dir, dest.name, arch)


def perform_merge_operation(root_dir, dest_dir):
    """Attempts to merge the SDKs using the merge script."""
    args = [
        sys.executable,
        MERGE_SCRIPT,
        "--source-dir",
        root_dir,
        "--dest-dir",
        dest_dir,
    ]
    print(args)
    print("INFO: merging sdk into {}".format(dest_dir))
    subprocess.check_call(args)


def parse_args():
    """Parses the command line arguments"""
    parser = argparse.ArgumentParser(
        description=
        "Creates a local merged Bazel SDK for all supported architectures.")
    parser.add_argument(
        "--output-dir",
        required=True,
        help="Output SDK directory. Will overwrite existing files if present")
    parser.add_argument(
        "--skip-build", help="Skips the build if set", action='store_true')

    args = parser.parse_args()

    return args


def main():
    args = parse_args()
    original_dir = os.getcwd()

    # Switch to the Fuchsia tree to ensure the fx commands work.
    os.chdir(REPOSITORY_ROOT)

    # remember our root build dir to switch back when we are done
    original_build_dir = get_current_build_dir(REPOSITORY_ROOT).strip()

    # Create a temporary directory where the merge will occur
    root_dir = tempfile.TemporaryDirectory()

    # Build the sdk for each architectures
    build_and_copy_all_sdks(root_dir, args.skip_build)

    # At this point we have all of our SDKs built and moved into the root_dir.
    # We can now run our merge operation.
    merged_dir = tempfile.TemporaryDirectory()
    perform_merge_operation(root_dir.name, merged_dir.name)

    # Copy the merged sdk to the output directory
    if args.output_dir:
        outdir = os.path.expanduser(args.output_dir)
        if not os.path.isabs(outdir):
            outdir = os.path.join(original_dir, outdir)
        print("INFO: copying built sdk to {}".format(outdir))
        shutil.copytree(merged_dir.name, outdir, dirs_exist_ok=True)

    # Clean up.
    os.chdir(original_dir)
    root_dir.cleanup()
    merged_dir.cleanup()

    # Reset our fx set
    subprocess.check_call(["fx", "use", original_build_dir])


if __name__ == "__main__":
    main()
