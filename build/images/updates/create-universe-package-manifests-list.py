#!/usr/bin/env fuchsia-vendored-python
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
from json.decoder import JSONDecodeError
import sys
from collections import OrderedDict


def read_manifest_list(manifest_list_file):
    manifest_list = OrderedDict()
    contents = manifest_list_file.read()
    if not contents:
        # `manifest_list_file` can be an empty file...
        return manifest_list

    manifest_list_object = json.loads(contents)
    for manifest_path in manifest_list_object['content']['manifests']:
        with open(manifest_path) as f:
            manifest = json.load(f)
            manifest_list[manifest['package']['name']] = manifest_path

    return manifest_list


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--depfile', help='If provided, create this depfile')
    parser.add_argument(
        '-o',
        '--output',
        help=
        'If provided, write to this output package manifest list rather than stdout'
    )
    parser.add_argument('base', help='base package manifest list')
    parser.add_argument('cache', help='cache package manifest list')
    parser.add_argument('metadata', help='metadata package manifest list')
    args = parser.parse_args()

    with open(args.base) as f:
        base_list = read_manifest_list(f)

    with open(args.cache) as f:
        cache_list = read_manifest_list(f)

    with open(args.metadata) as f:
        metadata_list = read_manifest_list(f)

    # make sure the cache package list doesn't have any base packages.
    for name, path in cache_list.items():
        if name in base_list:
            print(
                'package',
                name,
                'is both a base and cache package',
                file=sys.stderr)
            sys.exit(1)

    manifest_paths = []
    for name, path in metadata_list.items():
        if name in base_list or name in cache_list:
            continue

        manifest_paths.append(path)

    out_package_manifest_list = {
        'content': {
            'manifests': manifest_paths
        },
        'version': '1'
    }

    with open(args.output, 'w') as f:
        json.dump(out_package_manifest_list, f, indent=2, sort_keys=True)

    if args.depfile:
        with open(args.depfile, 'w') as f:
            deps = list(base_list.values())
            deps.extend(cache_list.values())
            deps.extend(metadata_list.values())

            f.write(f"{args.output}: {' '.join(deps)}\n")


if __name__ == "__main__":
    sys.exit(main())
