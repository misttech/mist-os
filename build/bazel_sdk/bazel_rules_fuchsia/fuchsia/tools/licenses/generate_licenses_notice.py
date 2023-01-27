#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
'''Utility that produces OSS licenses notice text file.'''

import argparse
import csv
import os
from sys import stderr
from typing import Set, List
import zipfile
from fuchsia.tools.licenses.spdx_types import *


def _log(*kwargs):
    print(*kwargs, file=stderr)


def main():
    '''Parses arguments.'''
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--spdx_input',
        help='An SPDX json file containing all licenses to process.'
        ' The output of @rules_fuchsia `fuchsia_licenses_spdx`',
        required=True,
    )
    parser.add_argument(
        '--output_file',
        help='Where to write the notice text file.',
        required=True,
    )
    args = parser.parse_args()

    _log(f'Got these args {args}!')

    spdx_input = args.spdx_input

    _log(f'Reading license info from {spdx_input}!')
    spdx_doc = SpdxDocument.from_json(spdx_input)
    spdx_index = SpdxIndex.create(spdx_doc)

    output_path = args.output_file
    _log(f'Writing {output_path}...')

    licenses_by_unique_text = defaultdict(list)
    for license in spdx_doc.extracted_licenses:
        licenses_by_unique_text[license.extracted_text].append(license)

    packages_by_unique_copyright = defaultdict(list)
    for package in spdx_doc.packages:
        if package.copyright_text:
            packages_by_unique_copyright[package.copyright_text].append(package)

    with open(output_path, 'w') as notice:

        def write(text):
            notice.write(text)

        def write_delimiter():
            write(
                '================================================================================\n'
            )

        for unique_text, licenses in licenses_by_unique_text.items():
            write_delimiter()
            write('The following license text(s) applies to these projects:\n')
            unique_package_names = set()
            for license in licenses:
                for package in spdx_index.get_packages_by_license(license):
                    unique_package_names.add(package.name)
            for package_name in sorted(list(unique_package_names)):
                write(f' • {package_name}\n')

            write('\n')
            write(unique_text)
            write('\n')

        for unique_text, packages in packages_by_unique_copyright.items():
            write_delimiter()

            write('The following copyright(s) applies to these projects:\n')
            unique_package_names = set([p.name for p in packages])
            for package_name in sorted(list(unique_package_names)):
                write(f' • {package_name}\n')

            write('\n')
            write(unique_text)
            write('\n')


if __name__ == '__main__':
    main()
