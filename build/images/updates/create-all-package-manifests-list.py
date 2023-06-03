#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os.path
import json
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', help='Write to this file')
    parser.add_argument('paths', nargs='+', help='package manifest lists')
    args = parser.parse_args()

    out = tempfile.NamedTemporaryFile(
        'w', dir=os.path.dirname(args.output), delete=False)
    try:
        manifest_paths = []
        for package_manifest_list_path in args.paths:
            with open(package_manifest_list_path, 'r') as f:
                package_manifest_list = json.load(f)
                manifest_paths.extend(
                    package_manifest_list['content']['manifests'])
        out_package_manifest_list = {
            'content': {
                'manifests': manifest_paths
            },
            'version': '1'
        }

        out.write(
            json.dumps(out_package_manifest_list, indent=2, sort_keys=True))
        out.close()

        os.replace(out.name, args.output)
    finally:
        try:
            os.unlink(out.name)
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    sys.exit(main())
