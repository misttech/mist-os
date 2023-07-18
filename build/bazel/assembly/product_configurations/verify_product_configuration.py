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
from typing import Any, Dict, List


def file_sha1(path):
    sha1 = hashlib.sha1()
    with open(path, "rb") as f:
        sha1.update(f.read())
    return sha1.hexdigest()


def normalize_file_in_config(
        configuration: Dict[str, Any], item: str, root_dir: str) -> None:
    """Replace an `item` (in "foo.bar.baz" format) in the `configuration` with
    an item that contains the sha1 of the file referenced. The new item will be
    suffixed with '_sha1'
    '"""
    # Split the item's path into a list of elements
    path_elements = item.split(".")
    node_path = path_elements[:-1]
    item_name = path_elements[-1]

    # Find the node (dict) that holds the item, or set it to None if not found.
    config_node = configuration
    for element in node_path:
        if element in config_node and config_node[element] is not None:
            config_node = config_node[element]
        else:
            # It's not here, so exit early.
            return

    if item_name in config_node and config_node[item_name] is not None:
        # External dependencies can be found by navigating into output_base.
        item_value = config_node[item_name]
        if item_value.startswith("external"):
            item_value = "../output_base/" + item_value

        # We've found the item to replace.
        # Rebase the path from the build root.
        file_path = os.path.join(root_dir, item_value)
        # Replace it with the hash of the file.
        config_node[f"{item_name}_sha1"] = file_sha1(file_path)
        config_node.pop(item_name)


def normalize_files_in_config(
        configuration: Dict[str, Any], items: List[str], root_dir: str) -> None:
    for item in items:
        normalize_file_in_config(configuration, item, root_dir)


def remove_empty_items(configuration: Dict[str, Any]) -> None:
    """Remove all items (recursively) whose value is 'None'
    """
    items_to_remove = []
    for (name, value) in configuration.items():
        # If the value is None, or the dict is now empty, also remove it.
        if value is None:
            items_to_remove.append(name)

        elif isinstance(value, Dict):
            # if the value is a dict, then remove any None-value and empty dicts
            # from it.
            remove_empty_items(value)

            # if it's now empty, remove the dict itself.
            if len(value) == 0:
                items_to_remove.append(name)

    # Now remove the items, after iterating over them all.
    for name in items_to_remove:
        configuration.pop(name)


def normalize_platform(config, root_dir):
    if "platform" not in config:
        return

    platform = config["platform"]

    # These are platform config items which are paths to files, but paths will
    # be different between GN and Bazel, so they need to be replaced with the
    # hash of the file to make sure that they're actually the same contents.
    #
    # This uses .append() instead of just setting it to a list so that if forces
    # the python auto-formatter to put one entry on each line, to reduce the
    # likelihood of merge conflicts.
    files_to_normalize = []
    files_to_normalize.append("development_support.authorized_ssh_keys_path")
    files_to_normalize.append(
        "development_support.authorized_ssh_ca_certs_path")
    files_to_normalize.append("ui.sensor_config")
    normalize_files_in_config(platform, files_to_normalize, root_dir)

    # Due to how some optional configs are routed to Bazel, there may be empty
    # sections in the configuration, this removes them all (recursively).
    remove_empty_items(platform)

    # When unset, set config_data to empty list for consistency, to avoid noisy
    # diff.
    if "diagnostics" not in platform:
        platform["diagnostics"] = {}
    if "additional_serial_log_components" not in platform["diagnostics"]:
        platform["diagnostics"]["additional_serial_log_components"] = []


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
