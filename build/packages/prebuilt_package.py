#!/usr/bin/env python3.8
# Copyright 2018 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import shutil
import subprocess
import sys


# Extract the package into the `args.workdir` directory, and returns the
# generated package manifest.
def extract(args):
    if not os.path.exists(args.workdir):
        os.makedirs(args.workdir)

    subprocess.check_call(
        [
            args.package_tool,
            'package',
            'archive',
            'extract',
            '-o',
            args.workdir,
            '--repository',
            args.repository,
            args.archive,
        ])

    return os.path.join(args.workdir, 'package_manifest.json')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--package-tool', help='Path to package tool')
    parser.add_argument('--name', help='Name of prebuilt package')
    parser.add_argument(
        '--archive', help='Path to archive containing prebuilt package')
    parser.add_argument('--workdir', help='Path to working directory')
    parser.add_argument('--repository', help='Repository host name')

    args = parser.parse_args()

    package_manifest = extract(args)

    # Make sure that the generated package matches the one we expected.
    with open(package_manifest) as f:
        manifest = json.load(f)
        if manifest.get('package').get('name') != args.name:
            sys.stderr.write(
                'Prebuilt package name {} does not match the name contained within {}\n'
                .format(args.name, args.archive))
            return 1

    return 0


if __name__ == '__main__':
    sys.exit(main())
