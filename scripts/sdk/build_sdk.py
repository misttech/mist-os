#!/usr/bin/env fuchsia-vendored-python

# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
This script serves as the entry point for building the merged GN SDK.
Due to complexities around the merging process, this can't be easily expressed
as a single GN target. This script thus serves as the canonical build script,
providing a single reproducible process.
"""

import atexit
import argparse
import json
import os
import shutil
import stat
import subprocess
import tarfile
import tempfile

import merger.merge as merge
import gn.generate as generate

REPOSITORY_ROOT = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", ".."))
FUCHSIA_FINT_PARAMS_DIR = os.path.join(
    REPOSITORY_ROOT, "integration", "infra", "config", "generated", "fuchsia",
    "fint_params", "global.ci")
TQ_FINT_PARAMS_DIR = os.path.join(
    REPOSITORY_ROOT, "integration", "infra", "config", "generated", "turquoise",
    "fint_params", "global.ci")
SUPPORTED_ARCHITECTURES = ["x64", "arm64"]
SDK_ARCHIVE_JSON = "sdk_archives.json"
RBE_BUILD_PARAMS = ["rust_rbe_enable", "cxx_rbe_enable"]
FINT_BOOTSTRAP_SCRIPT = os.path.join(
    REPOSITORY_ROOT, "tools", "integration", "bootstrap.sh")
FINT_OUTPUT = os.getenv("FX_CACHE_DIR", default="/tmp/fint")
FINT_BOOTSTRAP_COMMAND = [FINT_BOOTSTRAP_SCRIPT, "-o", FINT_OUTPUT]

FINT_CONTEXT_TEMPLATE = """checkout_dir: "{checkout_dir}"
build_dir: "{build_dir}"
"""

FUCHSIA_FINT_PARAMS_MAP = {
    "x64":
        os.path.join(
            FUCHSIA_FINT_PARAMS_DIR, "sdk-core-linux-x64-build_only.textproto"),
    "arm64":
        os.path.join(
            FUCHSIA_FINT_PARAMS_DIR,
            "sdk-core-linux-arm64-build_only.textproto")
}

INTERNAL_FINT_PARAMS_MAP = {
    "x64":
        os.path.join(
            TQ_FINT_PARAMS_DIR, "sdk-google-linux-x64-build_only.textproto"),
    "arm64":
        os.path.join(
            TQ_FINT_PARAMS_DIR, "sdk-google-linux-arm64-build_only.textproto")
}


def determine_build_dir(context_file):
    """
    Parses a context file to determine the build directory.
    See the context template above for an example.
    Infra builds will have more fields in the file.
    """
    with open(context_file) as context:
        for line in context.readlines():
            if line.startswith("build_dir: "):
                return line.lstrip("build_dir: ").strip(' "\n')


def build_for_arch(arch, fint, fint_params_files, rbe):
    """
    Does the build for an architecture, as defined by the fint param files.
    """
    print("Building {}".format(arch))

    # Read the build dir from the context file here for simplicity
    # even though sometimes it was set by the script in the first place.
    build_dir = determine_build_dir(fint_params_files["context"])

    # We have to rewrite the fint params as the base files have defaults for
    # rbe parameters, so those values have to be replaced based on the flags
    static_params_path = os.path.join(build_dir, "fint_params")
    with open(static_params_path, 'w') as params:
        with open(fint_params_files["static"]) as base_params:
            for line in base_params.readlines():
                if line.split(':')[0] in RBE_BUILD_PARAMS:
                    params.write("{}: {}\n".format(line.split(':')[0], rbe))
                else:
                    params.write(line)

    subprocess.check_call(
        [
            fint, "-log-level=error", "set", "-static", static_params_path,
            "-context", fint_params_files["context"]
        ])
    subprocess.check_call(
        [
            fint, "-log-level=error", "build", "-static", static_params_path,
            "-context", fint_params_files["context"]
        ])

    # The sdk archives file contains several entries since some SDKs build
    # others as intermediates, but only one should still be present.
    with open(os.path.join(build_dir, SDK_ARCHIVE_JSON)) as sdk_archive_json:
        sdk_archives = json.load(sdk_archive_json)
    for sdk in sdk_archives:
        if os.path.isfile(os.path.join(build_dir, sdk["path"])):
            return os.path.join(build_dir, sdk["path"])


def bootstrap_fint():
    subprocess.check_call(FINT_BOOTSTRAP_COMMAND)
    return FINT_OUTPUT


def generate_context(arch):
    """
    Generates an appropriate fint context file for a given architecture.
    """
    temp_context = "/tmp/context-{arch}".format(arch=arch)
    build_dir = os.path.join(REPOSITORY_ROOT, "out/release-{}".format(arch))
    with open(temp_context, "w") as context:
        context.write(
            FINT_CONTEXT_TEMPLATE.format(
                checkout_dir=REPOSITORY_ROOT, build_dir=build_dir))
    atexit.register(lambda: os.remove(temp_context))
    return temp_context


def build_fint_params(args):
    """
    Determines the appropriate fint params for a given architecture.
    Based on hardcoded values when architecture is provided as an argument.
    """
    fint_params = {}
    if args.fint_config:
        fint_params = json.loads(args.fint_config)
    elif args.fint_params_path:
        fint_params["custom"] = {
            "static": args.fint_params_path,
            "context": generate_context("custom")
        }
    elif args.arch:
        if args.internal:
            for arch in args.arch:
                fint_params[arch] = {
                    "static": INTERNAL_FINT_PARAMS_MAP[arch],
                    "context": generate_context(arch)
                }
        else:
            for arch in args.arch:
                fint_params[arch] = {
                    "static": FUCHSIA_FINT_PARAMS_MAP[arch],
                    "context": generate_context(arch)
                }

    return fint_params


def overwrite_even_if_RO(src, dst):
    """
    Copy function that tries to overwrite the destination file
    even if it's not writable.
    This is to support the use case where the same output directory is used
    more than once, as some files are read only.
    """
    if os.path.isfile(dst) and not os.access(dst, os.W_OK):
        os.chmod(dst, stat.S_IWUSR)
    shutil.copy2(src, dst)


def main():
    parser = argparse.ArgumentParser(
        description="Creates a GN SDK for a given architecture.")
    parser.add_argument(
        "--output",
        help="Output file path. If this file already exists, it will be replaced"
    )
    parser.add_argument(
        "--output-dir",
        help="Output SDK directory. Will overwrite existing files if present")
    parser.add_argument(
        "--fint-path", help="Path to fint", default=bootstrap_fint())
    parser.add_argument(
        "--GN", help="Apply GN build files to the IDK", action='store_true')
    parser.add_argument(
        "--internal",
        help="Build the google IDK if applicable (i.e. sdk-google-linux)",
        action='store_true')
    parser.add_argument(
        "--rbe", help="Enable remote build", action='store_true')
    build_params = parser.add_mutually_exclusive_group(required=True)
    build_params.add_argument(
        "--fint-config",
        help=
        "JSON with architectures and fint params. Use format {{arch: {{'static': static_path, 'context': context_path}}}}"
    )
    build_params.add_argument(
        "--arch",
        nargs="+",
        choices=SUPPORTED_ARCHITECTURES,
        help="Target architectures")
    build_params.add_argument(
        "--fint-params-path",
        help="Path of a single fint params file to use for this build")
    args = parser.parse_args()

    if not (args.output or args.output_dir):
        print(
            "Either an output file or an output directory should be specified.")
        return

    original_dir = os.getcwd()

    # Switch to the Fuchsia tree and build the SDKs.
    os.chdir(REPOSITORY_ROOT)

    output_dirs = []
    fint_params = build_fint_params(args)

    for arch in fint_params:
        sdk = build_for_arch(arch, args.fint_path, fint_params[arch], args.rbe)
        output_dirs.append(sdk)

    primary_dir = tempfile.TemporaryDirectory()
    with tarfile.open(output_dirs[0], 'r') as sdk:
        sdk.extractall(primary_dir.name)

    with tempfile.TemporaryDirectory() as tempdir_name:
        if len(output_dirs) > 1:
            print("Merging")
            for sdk in output_dirs[1:]:
                secondary_dir = tempfile.TemporaryDirectory()
                with tarfile.open(output_dirs[0], 'r') as sdk:
                    sdk.extractall(secondary_dir.name)
                output_dir = tempfile.TemporaryDirectory()
                merge.main(
                    [
                        "--input-directory",
                        primary_dir.name,
                        "--input-directory",
                        secondary_dir.name,
                        "--output-directory",
                        output_dir.name,
                    ])
                primary_dir.cleanup()
                secondary_dir.cleanup()
                primary_dir = output_dir
        sdk_output_dir = primary_dir.name

        if args.GN:
            # Process the Core SDK tarball to generate the GN SDK.
            print("Generating GN SDK")
            sdk_output_dir = tempdir_name
            if (generate.main([
                    "--directory",
                    primary_dir.name,
                    "--output",
                    tempdir_name,
            ])) != 0:
                print("Error - Failed to generate GN build files")
                primary_dir.cleanup()
                return 1

        if args.output:
            os.makedirs(os.path.dirname(args.output), exist_ok=True)
            generate.create_archive(args.output, sdk_output_dir)

        if args.output_dir:
            if not os.path.exists(args.output_dir) and not os.path.isfile(
                    args.output_dir):
                os.makedirs(args.output_dir)
            shutil.copytree(
                sdk_output_dir,
                args.output_dir,
                copy_function=overwrite_even_if_RO,
                dirs_exist_ok=True)

        primary_dir.cleanup()


# Clean up.
    os.chdir(original_dir)

if __name__ == "__main__":
    main()
