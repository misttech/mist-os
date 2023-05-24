#!/usr/bin/env fuchsia-vendored-python

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import difflib
import hashlib
import json
import os
import sys


def file_sha1(path):
    sha1 = hashlib.sha1()
    with open(path, "rb") as f:
        sha1.update(f.read())
    return sha1.hexdigest()


def normalize_platform(config, root_dir):
    if "platform" not in config:
        return

    platform = config["platform"]
    if "ui" in platform:
        ui = platform["ui"]
        if "sensor_config" in ui:
            p = os.path.join(root_dir, ui["sensor_config"])
            ui["sensor_config_sha1"] = file_sha1(p)
            ui.pop("sensor_config")

    # When unset, set config_data to empty list for consistency, to avoid noisy
    # diff.
    if "additional_serial_log_tags" not in platform:
        platform["additional_serial_log_tags"] = []


def normalize_product(
        config, root_dir, extra_files_read, config_data_to_ignore):
    if "product" not in config:
        return

    product = config["product"]

    if "packages" in product:
        packages = product["packages"]
        for pkg_set in ["base", "cache"]:
            if pkg_set not in packages:
                continue

            for pkg in packages[pkg_set]:
                p = os.path.join(root_dir, pkg["manifest"])
                # Follow links for depfile entry. See https://fxbug.dev/122513.
                p = os.path.relpath(os.path.realpath(p))
                with open(p, "r") as f:
                    manifest = json.load(f)
                    extra_files_read.append(p)
                    pkg["name"] = manifest["package"]["name"]
                    pkg["version"] = manifest["package"]["version"]

                # Skip comparison of manifest paths, because:
                #
                # 1. These paths are different
                # 2. Contents of these manifests can be different, because
                #    manifests contain paths to outputs from GN or Bazel
                # 3. Detailed comparison of blobs and other information in
                #    package manifests are possible, but non-trivial, so
                #    deferred to final assembly output comparison for now.
                pkg.pop("manifest", None)

                if "config_data" not in pkg:
                    # When unset, set config_data to empty list for consistency,
                    # to avoid noisy diff.
                    pkg["config_data"] = []
                    continue

                new_config_data = []
                for config_data in pkg["config_data"]:
                    pkg_name_and_destination = pkg["name"] + ":" + config_data[
                        "destination"]
                    if pkg_name_and_destination in config_data_to_ignore:
                        continue

                    # Config data source can have different paths, but they
                    # should have consistent content, so replace them with a
                    # file hash for comparison.
                    p = os.path.join(root_dir, config_data["source"])
                    config_data.pop("source", None)
                    config_data["package_name"] = pkg["name"]
                    # Follow links for depfile entry.
                    # See https://fxbug.dev/122513.
                    p = os.path.relpath(os.path.realpath(p))
                    config_data["source_sha1"] = file_sha1(p)
                    extra_files_read.append(p)
                    new_config_data.append(config_data)

                new_config_data.sort(key=lambda x: x["destination"])
                pkg["config_data"] = new_config_data

            packages[pkg_set].sort(key=lambda x: x["name"])

    if "drivers" in product:
        # TODO(jayzhuang): Normalize `drivers` field when we have product
        # configs with this field set for comparison.
        pass

    return


def normalize(config, root_dir, extra_files_read, config_data_to_ignore):
    normalize_platform(config, root_dir)
    normalize_product(config, root_dir, extra_files_read, config_data_to_ignore)


def main():
    parser = argparse.ArgumentParser(
        description="Compares assembly product configurations")
    parser.add_argument(
        "--product_config1", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--root_dir1",
        help="Directory where paths in --product_config1 are relative to",
        required=True)
    parser.add_argument(
        "--product_config2", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--root_dir2",
        help="Directory where paths in --product_config2 are relative to",
        required=True)
    parser.add_argument("--depfile", type=argparse.FileType("w"), required=True)
    parser.add_argument(
        "--config_data_to_ignore",
        nargs='*',
        default=[],
        help="""List of config data entries that the verification should ignore.
            The entries should be of the form [package_name]:[destination]""",
        required=False)
    parser.add_argument("--output", type=argparse.FileType("w"), required=True)

    args = parser.parse_args()

    product_config_json1 = json.load(args.product_config1)
    product_config_json2 = json.load(args.product_config2)

    extra_files_read = []
    normalize(
        product_config_json1, args.root_dir1, extra_files_read,
        args.config_data_to_ignore)
    normalize(
        product_config_json2, args.root_dir2, extra_files_read,
        args.config_data_to_ignore)

    canon1 = json.dumps(
        product_config_json1, sort_keys=True, indent=2).splitlines()
    canon2 = json.dumps(
        product_config_json2, sort_keys=True, indent=2).splitlines()

    diff = difflib.unified_diff(
        canon1,
        canon2,
        args.product_config1.name,
        args.product_config2.name,
        lineterm="")
    diffstr = "\n".join(diff)
    args.output.write(diffstr)

    args.depfile.write(
        "{}: {}".format(args.output.name, ' '.join(extra_files_read)))

    if (len(diffstr) != 0):
        print(f"Error: non-empty diff product configs:\n{diffstr}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
