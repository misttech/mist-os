#!/usr/bin/env fuchsia-vendored-python
"""Creates a Python library for a C extension."""

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os
import sys


def main():
    """Creates library wrapper"""

    parser = argparse.ArgumentParser(
        'Creates a Python library wrapper for a C extension.')
    parser.add_argument(
        '--target_name',
        help='Name of the build target',
        required=True,
    )
    parser.add_argument(
        '--shlib_source',
        help='Path to the shared library',
        required=True,
    )
    parser.add_argument(
        '--gen_dir',
        help='Path to gen directory, used to stage temp directories',
        required=True,
    )
    parser.add_argument('--output', help='Path to output', required=True)
    parser.add_argument(
        '--depfile',
        help='Path to the depfile to generate',
        type=argparse.FileType('w'),
        required=True,
    )

    args = parser.parse_args()
    app_dir = os.path.join(args.gen_dir, args.target_name)
    os.makedirs(app_dir, exist_ok=True)
    main_file = os.path.join(app_dir, "__init__.py")
    shlib_source_dir = os.path.dirname(args.shlib_source)
    if not shlib_source_dir:
        shlib_source_dir = os.path.join('.')
    shlib_source = os.path.basename(args.shlib_source)

    # Making this a list in the event that there are more deps.
    deps = ' '.join([f'{args.shlib_source}.so'])
    args.depfile.write(f'{args.output}: {deps}\n')
    with open(main_file, 'w', encoding='utf-8') as main_file_out:
        main_file_out.write(
            f'''
import importlib.util
import importlib.machinery

def _init():
    finder = importlib.machinery.PathFinder()
    spec = finder.find_spec('{shlib_source}', path=['{shlib_source_dir}'])
    if spec is None:
        raise Exception('Couldn\\'t load library "{shlib_source}" from {shlib_source_dir}')
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod

__all__ = _init().__dict__
''')
    return 0


if __name__ == '__main__':
    sys.exit(main())
