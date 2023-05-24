#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generate a hermetic inputs file for image assembly by reading the image
assembly inputs and generating a list of all the files that are read"""

import argparse
import json
import os
import sys
from typing import List, Set

from depfile import DepFile
from assembly import FileEntry, FilePath, ImageAssemblyConfig, PackageManifest
from serialization import json_load


def get_relative_path(relative_path: str, relative_to_file: str) -> str:
    file_parent = os.path.dirname(relative_to_file)
    path = os.path.join(file_parent, relative_path)
    path = os.path.realpath(path)
    path = os.path.relpath(path, os.getcwd())
    return path


def files_from_package_set(package_set: List[FilePath],
                           deps: Set[FilePath]) -> Set[FilePath]:
    paths: Set[FilePath] = set()
    for manifest in package_set:
        paths.add(manifest)
        with open(manifest, 'r') as file:
            package_manifest = json_load(PackageManifest, file)
            blob_sources = []
            for blob in package_manifest.blobs:
                path = blob.source_path
                if package_manifest.blob_sources_relative:
                    path = get_relative_path(path, manifest)
                blob_sources.append(path)
            paths.update(blob_sources)
            if package_manifest.subpackages:
                subpackage_set = []
                for subpackage in package_manifest.subpackages:
                    path = subpackage.manifest_path
                    if package_manifest.blob_sources_relative:
                        path = get_relative_path(path, manifest)
                    if path not in deps:
                        subpackage_set.append(path)
                paths.update(subpackage_set)
                deps.update(subpackage_set)
                paths.update(files_from_package_set(subpackage_set, deps))
    return paths


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        '--image-assembly-config',
        type=argparse.FileType('r'),
        required=True,
        help='The path to the image assembly config file')
    parser.add_argument(
        '--images-config',
        type=argparse.FileType('r'),
        help='The path to the image assembly images config file')
    parser.add_argument(
        '--output',
        type=str,
        required=True,
        help='The path to the first output of the image assembly target')
    parser.add_argument(
        '--depfile',
        type=argparse.FileType('w'),
        required=True,
        help='The path to the depfile for this script')

    args = parser.parse_args()

    config = ImageAssemblyConfig.json_load(args.image_assembly_config)

    # Collect the list of files that are read in this script.
    deps: Set[FilePath] = set()
    deps.update(config.base)
    deps.update(config.cache)
    deps.update(config.system)
    deps.update(config.bootfs_packages)

    # Collect the list of inputs to image assembly.
    inputs: Set[FilePath] = set()
    inputs.update(files_from_package_set(config.base, deps))
    inputs.update(files_from_package_set(config.cache, deps))
    inputs.update(files_from_package_set(config.system, deps))
    inputs.update(files_from_package_set(config.bootfs_packages, deps))
    inputs.update([entry.source for entry in config.bootfs_files])
    inputs.add(config.kernel.path)

    if deps:
        dep_file = DepFile(args.output)
        dep_file.update(deps)
        dep_file.write_to(args.depfile)

    if args.images_config:
        images_config = json.load(args.images_config)['images']
        for image in images_config:
            if image['type'] == 'vbmeta':
                if 'key' in image:
                    inputs.add(image['key'])
                if 'key_metadata' in image:
                    inputs.add(image['key_metadata'])
                inputs.update(image.get('additional_descriptor_files', []))
            elif image['type'] == 'zbi':
                if 'postprocessing_script' in image:
                    script = image['postprocessing_script']
                    if 'path' in script:
                        inputs.add(script['path'])

    with open(args.output, 'w') as f:
        for input in inputs:
            f.write(input + '\n')


if __name__ == '__main__':
    sys.exit(main())
