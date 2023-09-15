# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tool to check that the fuchsia package was built correctly."""

import argparse
import json
import subprocess
import tempfile
import sys


def parse_args():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--meta_far",
        help="Path to the meta.far file",
        required=True,
    )
    parser.add_argument(
        "--far",
        help="Path to the far tool",
        required=True,
    )
    parser.add_argument(
        "--ffx",
        help="Path to the ffx tool",
        required=True,
    )
    parser.add_argument(
        "--manifests",
        help="The expected component manifest paths (meta/foo.cm)",
        action="append",
        default=[],
        required=False,
    )
    parser.add_argument(
        "--bind_bytecode",
        help="The expected bind bytecode paths (meta/bind/foo.bindbc)",
        default=None,
        required=False,
    )
    parser.add_argument(
        "--package_name",
        help="Expected name of the package",
        required=True,
    )
    parser.add_argument(
        "--blobs",
        help="The exected blobs in dest=src format",
        action="append",
        default=[],
        required=False,
    )
    return parser.parse_args()


def run(*command):
    try:
        return subprocess.check_output(
            command,
            text=True,
        ).strip()
    except subprocess.CalledProcessError as e:
        print(e.stdout)
        raise e


def _assert_in(value, iterable, msg):
    if not value in iterable:
        print("{}, {} not found in {}".format(msg, value, iterable))
        sys.exit(1)


def _assert_eq(a, b, msg):
    if a != b:
        print(Exception("{}: {} != {}".format(msg, a, b)))
        sys.exit(1)


def dest_merkle_pair_for_blobs(blobs, ffx):
    if len(blobs) == 0:
        return []

    temp_dir = tempfile.TemporaryDirectory()

    srcs = [blob.split("=")[1] for blob in blobs]
    merkles = [
        v.split(" ")[0].strip() for v in run(
            ffx, "--isolate-dir", temp_dir.name, "package", "file-hash", *
            srcs).split("\n")
    ]

    pairs = []
    for i, blob in enumerate(blobs):
        dest = blob.split("=")[0]
        pairs.append("{}={}".format(dest, merkles[i]))

    return pairs


def list_contents(args):
    return run(
        args.far,
        "list",
        "--archive=" + args.meta_far,
    ).split("\n")


def check_contents_for_component_manifests(contents, manifests):
    for manifest in manifests:
        _assert_in(manifest, contents, "Failed to find component manifest")


def check_contents_for_bind_bytecode(contents, bind):
    if bind:
        _assert_in(bind, contents, "Failed to find bind bytecode")


def check_for_abi_revision(contents):
    #TODO: we should actually check the contents of this file to see if it is valid.
    _assert_in(
        "meta/fuchsia.abi/abi-revision", contents,
        "Failed to find abi-revision file")


def check_package_name(args):
    contents = json.loads(
        run(
            args.far, "cat", "--archive=" + args.meta_far,
            "--file=meta/package"))

    _assert_eq(
        contents["name"], args.package_name, "Package name does not match")


def check_package_has_all_blobs(args):
    dest_to_merkle = dest_merkle_pair_for_blobs(args.blobs, args.ffx)

    contents = run(
        args.far, "cat", "--archive=" + args.meta_far,
        "--file=meta/contents").split("\n")

    _assert_eq(
        sorted(contents), sorted(dest_to_merkle),
        "Expected blobs not in package contents")


def main():
    args = parse_args()
    contents = list_contents(args)

    # TODO: add components as an arg
    check_contents_for_component_manifests(contents, args.manifests)
    check_contents_for_bind_bytecode(contents, args.bind_bytecode)
    check_for_abi_revision(contents)
    check_package_name(args)
    check_package_has_all_blobs(args)


if __name__ == "__main__":
    main()
