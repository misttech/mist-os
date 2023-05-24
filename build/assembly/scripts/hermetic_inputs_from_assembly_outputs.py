#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import sys
import os

from assembly import FilePath, PackageManifest
from depfile import DepFile
from serialization import json_load
from typing import List, Set


def get_relative_path(relative_path: str, relative_to_file: str) -> str:
    file_parent = os.path.dirname(relative_to_file)
    path = os.path.join(file_parent, relative_path)
    path = os.path.relpath(path, os.getcwd())
    return path


def add_inputs_from_packages(
        package_paths: Set[FilePath],
        all_manifest_paths: Set[FilePath],
        inputs: List[FilePath],
        include_blobs: bool,
        in_subpackage: bool = False):
    anonymous_subpackages: Set[FilePath] = set()
    for manifest_path in package_paths:
        inputs.append(manifest_path)

        with open(manifest_path, 'r') as f:
            package_manifest = json_load(PackageManifest, f)

        # Loading file-relative subpackages ends up statting blobs in the
        # subpackages, so we need to also mark that they get accessed.
        if include_blobs or (in_subpackage and
                             package_manifest.blob_sources_relative == 'file'):
            for blob in package_manifest.blobs:
                blob_source = blob.source_path

                if package_manifest.blob_sources_relative == 'file':
                    blob_source = get_relative_path(blob_source, manifest_path)

                inputs.append(blob_source)

        for subpackage in package_manifest.subpackages:
            subpackage_path = subpackage.manifest_path

            if package_manifest.blob_sources_relative == 'file':
                subpackage_path = get_relative_path(
                    subpackage_path, manifest_path)

            if not subpackage_path in all_manifest_paths:
                anonymous_subpackages.add(subpackage_path)
                all_manifest_paths.add(subpackage_path)

    if anonymous_subpackages:
        add_inputs_from_packages(
            anonymous_subpackages,
            all_manifest_paths,
            inputs,
            include_blobs,
            in_subpackage=True)


def main():
    parser = argparse.ArgumentParser(
        description=
        'Generate a hermetic inputs file that includes the outputs of Assembly')
    parser.add_argument(
        '--partitions',
        type=argparse.FileType('r'),
        required=True,
        help=
        'The partitions config that follows this schema: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/developer/ffx/plugins/assembly/#partitions-config'
    )
    parser.add_argument(
        '--output',
        type=argparse.FileType('w'),
        required=True,
        help='The location to write the hermetic inputs file')
    # TODO(fxbug.dev/110940): Avoid including transitive dependencies (blobs).
    parser.add_argument(
        '--include-blobs',
        action='store_true',
        help='Whether to include packages and blobs')
    parser.add_argument(
        '--system',
        type=argparse.FileType('r'),
        nargs='*',
        help=
        'A list of system image manifests that follow this schema: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/developer/ffx/plugins/assembly/#images-manifest'
    )
    parser.add_argument(
        '--depfile',
        type=argparse.FileType('w'),
        help='A depfile listing all the files opened by this script')
    args = parser.parse_args()

    # A list of the implicit inputs.
    inputs = []

    # Add all the bootloaders as inputs.
    partitions = json.load(args.partitions)
    for bootloader in partitions.get('bootloader_partitions', []):
        inputs.append(bootloader['image'])
    for bootstrap in partitions.get('bootstrap_partitions', []):
        inputs.append(bootstrap['image'])
    for credential in partitions.get('unlock_credentials', []):
        inputs.append(credential)

    # Add all the system images as inputs.
    package_manifest_paths: Set[FilePath] = set()
    for image_manifest_file in args.system:
        image_manifest = json.load(image_manifest_file)
        manifest_path = os.path.dirname(image_manifest_file.name)
        for image in image_manifest:
            inputs.append(os.path.join(manifest_path, image['path']))

            # Collect the package manifests from the blobfs image.
            if image['type'] == 'blk' and image['name'] == 'blob':
                packages = []
                packages.extend(image['contents']['packages'].get('base', []))
                packages.extend(image['contents']['packages'].get('cache', []))
                package_manifest_paths.update(
                    [
                        os.path.join(manifest_path, package['manifest'])
                        for package in packages
                    ])

    # If we collected any package manifests, include all the blobs referenced
    # by them.
    all_manifest_paths: Set[FilePath] = set(package_manifest_paths)
    add_inputs_from_packages(
        package_manifest_paths,
        all_manifest_paths,
        inputs,
        include_blobs=args.include_blobs)

    # Write the hermetic inputs file.
    args.output.writelines(f"{input}\n" for input in sorted(inputs))

    # Write the depfile.
    if args.depfile:
        DepFile.from_deps(args.output.name,
                          all_manifest_paths).write_to(args.depfile)


if __name__ == '__main__':
    sys.exit(main())
