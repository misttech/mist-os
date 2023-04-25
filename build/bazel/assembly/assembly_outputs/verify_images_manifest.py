#!/usr/bin/env python3.8

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Verify the Bazel Assembly-produced images.json vs. the existing GN version"""

import argparse
import hashlib
import json
import os
import re
import sys

from itertools import zip_longest


def file_sha1(path):
    """SHA1 hash a file"""
    sha1 = hashlib.sha1()
    with open(path, "rb") as f:
        sha1.update(f.read())
    return sha1.hexdigest()


def get_file_hash(path, root_dir, extra_files_read):
    """Get a SHA1 hash value of a file, and update extra_files_read"""
    p = os.path.join(root_dir, path)
    hash_value = file_sha1(p)
    # Follow links for depfile entry. See https://fxbug.dev/122513.
    p = os.path.relpath(os.path.realpath(p))
    extra_files_read.append(p)
    return hash_value


def image_key(image):
    """Sort key for images in the images config"""
    return image["path"]


def package_key(package):
    """Sort key for packages in the blob image"""
    return package["name"]


def strip_manifest_path(package):
    package["manifest"] = ""
    return package


def normalize(
        config, root_dir, exclude_packages, exclude_images, extra_files_read):
    """
    Clean up the input for diffing.
    """
    for image in config:
        if not image["path"]:
            raise ValueError(
                "Paths should not be missing from images.json entries",
                json.dumps(image))

        if os.path.basename(image["path"]) not in exclude_images:
            image["path_hash"] = get_file_hash(
                image["path"], root_dir, extra_files_read)

        # Replace with the full path to the basename of the file. This is because
        # an images.json may list multiple filesystems of the same type
        # differentiated only by their image file. We want to disregard
        # the files location and consider only the name for the purposes
        # of sorting the list by item for diffing.
        image["path"] = os.path.basename(image["path"])

        if image.get("name") == "blob":
            pkgs = image["contents"]["packages"]
            for pkg_set in ('base', 'cache'):
                # Sort the blobs in-place for when we print the file for
                # debugging.
                pkgs[pkg_set].sort(key=package_key)

                pkgs[pkg_set] = [
                    # We can ignore the manifest for now and just make sure the
                    # blobs are the same.
                    strip_manifest_path(pkg)
                    for pkg in pkgs[pkg_set]
                    if pkg["name"] not in exclude_packages
                ]

    config.sort(key=image_key)

    return config


def json_format(x):
    """
    JSON format an object with default parameters

    This is just for readability, as we need this for diffing and printing
    things nicely in our file.
    """
    return json.dumps(x, indent=2, sort_keys=True)


def get_diffs(items1, items2, keyf):
    """
    Sort the input arrays (nlogn) and diff them in linear time.

    This is instead of using difflib which is worst-case quadratic and is
    in practice too slow on the images file.
    """
    items1 = sorted(items1, key=keyf)
    items2 = sorted(items2, key=keyf)
    idx1 = 0
    idx2 = 0

    diffs = []

    while idx1 < len(items1) and idx2 < len(items2):
        key1 = keyf(items1[idx1])
        key2 = keyf(items2[idx2])

        if key1 != key2:
            diffs.append(
                (
                    json_format(items1[idx1]) if key1 < key2 else "{}",
                    json_format(items2[idx2]) if key2 < key1 else "{}"))

            if key1 < key2:
                idx1 += 1
            else:
                idx2 += 1
        else:
            item1 = json_format(items1[idx1])
            item2 = json_format(items2[idx2])
            if item1 != item2:
                diffs.append((item1, item2))

            idx1 += 1
            idx2 += 1

    if idx1 < len(items1):
        diffs += list(map(lambda x: ("{}", json_format(x)), items1[idx1:]))

    if idx2 < len(items2):
        diffs += list(map(lambda x: (json_format(x), "{}"), items2[idx2:]))

    return diffs


def get_first(predicate, sequence):
    """Get the first item in the sequence where the predicate is true"""
    return next(filter(predicate, sequence), None)


def format_diffs(diffs):
    """Format found diffs for printing"""
    formatted_diffs = list(map("\n---\n".join, diffs))
    return "Diffs found:\n" + "<<<<\n" + \
        "\n>>>>\n<<<<\n".join(formatted_diffs) + "\n>>>>"


def main():
    parser = argparse.ArgumentParser(
        description="Compares assembly images configurations")
    parser.add_argument(
        "--images_manifest_gn", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--images_manifest_bzl", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--path-mapping", type=argparse.FileType("r"), required=True)
    parser.add_argument("--depfile", type=argparse.FileType("w"), required=True)
    parser.add_argument("--output1", type=argparse.FileType("w"), required=True)
    parser.add_argument("--output2", type=argparse.FileType("w"), required=True)
    parser.add_argument("--exclude-packages", required=False, nargs="*")
    parser.add_argument("--exclude-images", required=False, nargs="*")

    args = parser.parse_args()

    images_manifest_gn = json.load(args.images_manifest_gn)
    images_manifest_bzl = json.load(args.images_manifest_bzl)

    extra_files_read = []
    exclude_packages = []
    if args.exclude_packages:
        exclude_packages = args.exclude_packages

    exclude_images = []
    if args.exclude_images:
        exclude_images = args.exclude_images

    bazel_images_manifest_dir = ""
    for line in args.path_mapping:
        gn_path, bazel_path = line.split(":")
        if gn_path.endswith('_create_system'):
            bazel_images_manifest_dir = bazel_path
            break

    normalize(
        images_manifest_gn, os.path.dirname(args.images_manifest_gn.name),
        exclude_packages, exclude_images, extra_files_read)

    normalize(
        images_manifest_bzl, bazel_images_manifest_dir, exclude_packages,
        exclude_images, extra_files_read)

    images_manifest_gn_str = json_format(images_manifest_gn)
    images_manifest_bzl_str = json_format(images_manifest_bzl)

    # Write the cleaned-up output out to files so we can diff them outside the
    # test with some other tool
    args.output1.write(images_manifest_gn_str)
    args.output2.write(images_manifest_bzl_str)

    args.depfile.write(
        "{}: {}".format(args.output1.name, " ".join(extra_files_read)))

    # These files are very large and single-threaded difflib is too slow when in the quadratic case

    # First diff the package blobs, if they're there.
    blob1 = get_first(lambda image: image["name"] == "blob", images_manifest_gn)
    blob2 = get_first(
        lambda image: image["name"] == "blob", images_manifest_bzl)
    if blob1 and blob2:
        diffs = get_diffs(
            blob1["contents"]["packages"]["base"] +
            blob1["contents"]["packages"]["cache"],
            blob2["contents"]["packages"]["base"] +
            blob2["contents"]["packages"]["cache"],
            keyf=lambda package: package["name"])

        if diffs:
            print(format_diffs(diffs))
            return 1

    # If there weren't any diffs in the package blobs, diff the whole thing
    diffs = get_diffs(images_manifest_gn, images_manifest_bzl, keyf=image_key)

    if diffs:
        print(format_diffs(diffs))
        return 1

    # Diff the whole file text just in case. This should never fail,
    # as we should have covered all diffs above -- if it does there's a bug
    # in the diff logic.
    for line_pair in zip_longest(images_manifest_gn_str.splitlines(),
                                 images_manifest_bzl_str.splitlines()):
        if line_pair[0] != line_pair[1]:
            print(f"Unexpected diff found: {line_pair}")
            print(
                f"Please check {args.output1.name} and {args.output2.name} to compare the normalized outputs."
            )
            return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
