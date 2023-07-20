#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generates the necessary files to provide an sdk_atom() target
for including a package in the SDK, and writes these to the output
directory. See `//build/sdk/sdk_atom.gni` for exact description of each file.

Files include:
* `file_path`
* `metadata`

Additionally, each package manifest has their `source_path` entries altered
such that both blobs and subpackage manifests point to their new SDK locations.
These final package manifests are also written to the output directory.
For use in sdk_fuchsia_package() template."""

import argparse
import json
import os
import sys

# SDK directory of blobs across all package manifests,
# each renamed to their merkle.
BLOBS_DIR = "fuchsia_packages/blobs"

# SDK directory of subpackage manifests, each renamed to their merkle.
SUBPACKAGE_MANIFEST_DIR = "fuchsia_packages/subpackage_manifests"

# sdk://
# └── fuchsia_packages
#     ├── blobs
#     │   └── CONTENT_HASH_1
#     ├── PACKAGE_BAR
#     │   ├── ARCH
#     │   │   └── release
#     │   │       └── package_manifest.json
#     │   └── meta.json
#     └── subpackage_manifests
#         └── META_FAR_MERKLE.package_manifest.json
# SDK directory of blobs, relative to each SDK package.
PACKAGE_DIR_TO_BLOBS_DIR = "../../../blobs"
# SDK directory of blobs, relative to `subpackage_manifests`.
SUBPACKAGE_MANIFESTS_DIR_TO_BLOBS_DIR = "../blobs"
# SDK directory of subpackage manifests, relative to individual SDK packages.
PACKAGE_DIR_TO_SUBPACKAGE_MANIFESTS_DIR = "../../../subpackage_manifests"


def handle_package_manifest(
        output_dir,
        input_manifest_path,
        sdk_file_map,
        sdk_metadata,
        depfile_collection,
        inputs,
        visited_subpackages,
        is_subpackage=False):
    """
    For the given `input_manifest_path`, does the following:
    * Re-writes all source paths to be relative to SDK location,
      where relative location is determined by if it is a subpackage.
    * Adds files for inclusion to the `sdk_atom`'s `file_list`.
    * Adds files and base package manifest to the `sdk_atom`'s
      `metadata`.
    * Adds files to `depfile`.
    * Writes re-written package manifest to desired `output_dir` location.

    Above is recursed across all subpackages.

    Args:
        output_dir:          Path to build directory to write final package
                             manifest filepath to.
        input_manifest_path: Path to package manifest to convert into
                             SDK-friendly format.
        sdk_file_map:        Set containing <dst>=<src> entries, for use in
                             `sdk_atom`'s `file_list`.
        sdk_metadata:        Object used to build a
                             `//build/sdk/meta/package.json` entry.
        depfile_collection:  Object constructed for use as a depfile.
        inputs:              Object containing `api_level`, `arch`, and
                             `distribution_name`. Used for constructing end
                             paths and other structures.
        visited_subpackages: Set used for tracking levels of recursion in
                             subpackages.
        is_subpackage:       Boolean used to track if this iteration is processing
                             a subpackages. Used to determine if `sdk_metadata`
                             changes are required, as well as pathing.
    """
    api_level, arch, distribution_name = inputs['api_level'], inputs[
        'arch'], inputs['distribution_name']

    with open(input_manifest_path, 'r') as manifest_file:
        input_manifest = json.load(manifest_file)

    # Re-wire will be relative to package manifest location.
    input_manifest["blob_sources_relative"] = "file"

    # Subpackage manifests have different relative paths.
    relative_blobs_dir = SUBPACKAGE_MANIFESTS_DIR_TO_BLOBS_DIR if is_subpackage else PACKAGE_DIR_TO_BLOBS_DIR
    relative_subpackage_manifests_dir = "" if is_subpackage else PACKAGE_DIR_TO_SUBPACKAGE_MANIFESTS_DIR

    # `meta_far_merkle` necessary for naming subpackage manifests.
    meta_far_merkle = ""
    for blob in input_manifest['blobs']:
        merkle, source_path = blob['merkle'], blob['source_path']
        relative_blob_path = f"{relative_blobs_dir}/{merkle}"

        if blob['path'] == "meta/":
            meta_far_merkle = merkle

        # Re-wire source path to point to SDK blob store.
        blob['source_path'] = relative_blob_path

        sdk_file_map.add(f"{BLOBS_DIR}/{merkle}={source_path}")
        sdk_metadata['target_files'][arch].append(f"{BLOBS_DIR}/{merkle}")

    # Handle subpackages.
    subpackage_list = input_manifest.get('subpackages', [])
    for subpackage in subpackage_list:
        subpackage_manifest_path, subpackage_merkle = subpackage[
            'manifest_path'], subpackage['merkle']
        # Re-wire subpackage manifest paths.
        subpackage[
            'manifest_path'] = f"{relative_subpackage_manifests_dir}/{subpackage_merkle}"

        if subpackage_manifest_path in visited_subpackages:
            # No need to re-visit same subpackage multiple times.
            continue

        # Recursively handle subpackages.
        handle_package_manifest(
            output_dir,
            subpackage_manifest_path,
            sdk_file_map,
            sdk_metadata,
            depfile_collection,
            inputs,
            visited_subpackages,
            is_subpackage=True)

    if is_subpackage:
        manifest_file_name = f"{meta_far_merkle}.package_manifest.json"
        sdk_output_manifest_path = f"{SUBPACKAGE_MANIFEST_DIR}/{manifest_file_name}"

        visited_subpackages.add(input_manifest_path)
    else:
        manifest_file_name = "package_manifest.json"

        sdk_output_manifest_path = f"fuchsia_packages/{distribution_name}/{arch}/release/{manifest_file_name}"

        # Ensure metadata is aware of SDK manifest location.
        sdk_metadata['package_manifests'] = [
            {
                "manifest_file": sdk_output_manifest_path,
                "target_architecture": arch,
                "api_level": api_level,
            }
        ]
        # Ensure package name matches distribution name.
        input_manifest['package']['name'] = distribution_name

    build_output_manifest_path = f"{output_dir}/{manifest_file_name}"

    sdk_file_map.add(f"{sdk_output_manifest_path}={build_output_manifest_path}")
    sdk_metadata['target_files'][arch].append(sdk_output_manifest_path)

    # Write altered manifest to build output location.
    with open(build_output_manifest_path, "w") as manifest_file:
        json.dump(input_manifest, manifest_file, indent=2)
        depfile_collection[build_output_manifest_path] = [input_manifest_path]


def main():
    parser = argparse.ArgumentParser(description=__doc__)

    parser.add_argument(
        '--distribution-name',
        help='Name of package for publication in the SDK.',
        required=True)
    parser.add_argument(
        '--manifest', help='Path to the package manifest.', required=True)
    parser.add_argument(
        '--sdk-package-content-checklist-path',
        help='Path to the SDK package content checklist file.',
        required=True)
    parser.add_argument(
        '--output', help='Directory to write SDK objects to.', required=True)
    parser.add_argument('--api-level', help='Target API level.', required=True)
    parser.add_argument(
        '--target-cpu', help='Target build architecture.', required=True)
    parser.add_argument(
        '--depfile', help='Path for generating depfile.', required=True)

    args = parser.parse_args()

    # File containing file mappings. Each line in the file should contain a
    # "dest=source" mapping, similarly to file scopes.
    # See `sdk_atom` template definition of `file_list`.
    sdk_file_map = set()

    # Inputs used for directory naming.
    api_level, arch, distribution_name = args.api_level, args.target_cpu, args.distribution_name
    inputs = {
        "api_level": int(api_level),
        "arch": arch,
        "distribution_name": distribution_name,
    }

    # See `sdk_atom` template definition of `metadata`.
    sdk_metadata = {
        "name": distribution_name,
        "package_manifests": {},
        "target_files": {
            arch: []
        },
        "type": "package",
    }

    depfile_collection = {}
    visited_subpackages = set()

    handle_package_manifest(
        args.output,
        args.manifest,
        sdk_file_map,
        sdk_metadata,
        depfile_collection,
        inputs,
        visited_subpackages,
        is_subpackage=False)

    # Capture content checklist file in metadata and file list.
    content_checklist_path = f"fuchsia_packages/{distribution_name}/{arch}/release/content_checklist_path"
    sdk_file_map.add(
        f"{content_checklist_path}={args.sdk_package_content_checklist_path}")
    sdk_metadata['target_files'][arch].append(content_checklist_path)

    # Handle license file.
    sdk_package_license_build_path = os.path.join(
        args.output, "license_data", "results.spdx.json")
    sdk_package_license_path = f"fuchsia_packages/{distribution_name}/{arch}/release/license.spdx.json"
    sdk_file_map.add(
        f"{sdk_package_license_path}={sdk_package_license_build_path}")
    sdk_metadata['target_files'][arch].append(sdk_package_license_path)

    # Write out sorted file list.
    sdk_file_list = sorted(list(sdk_file_map))
    sdk_file_list_path = os.path.join(args.output, "file_list.fini")
    with open(sdk_file_list_path, 'w') as out_file:
        out_file.write('\n'.join(sdk_file_list))
        out_file.write('\n')

    # Write out metadata.
    metadata_path = os.path.join(args.output, "metadata")
    sdk_metadata['target_files'][arch] = sorted(
        list(set(sdk_metadata['target_files'][arch])))
    with open(metadata_path, "w") as metadata_file:
        json.dump(
            sdk_metadata,
            metadata_file,
            indent=2,
            sort_keys=True,
            separators=(',', ': '))

    # Write out depfile
    if args.depfile:
        os.makedirs(os.path.dirname(args.depfile), exist_ok=True)
        with open(args.depfile, "w") as f:
            for out_file in sorted(depfile_collection.keys()):
                in_file_list = sorted(depfile_collection[out_file])
                f.write(f"{out_file}: {' '.join(in_file_list)}")

    return 0


if __name__ == '__main__':
    sys.exit(main())
