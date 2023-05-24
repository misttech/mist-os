#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generates an API definition for inclusion in the IDK by reading the passed
package manifest, files (with specified dispositions), and reference to compare
with. For use in sdk_fuchsia_package() template."""

import argparse
import collections
import difflib
import json
import os
import platform
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        '--manifest', help='Path to the package manifest.', required=True)
    parser.add_argument(
        '--output', help='Path to the API file to compute.', required=True)
    parser.add_argument(
        '--expected-files-exact',
        action='append',
        help='Exact files to capture.',
        required=False,
        default=[])
    parser.add_argument(
        '--expected-files-internal',
        action='append',
        help='internal files to capture.',
        required=False,
        default=[])
    parser.add_argument(
        '--reference', help='Path to the golden API file', required=False)
    parser.add_argument(
        '--warn',
        help='Whether API changes should only cause warnings',
        action='store_true')

    args = parser.parse_args()

    if not os.path.isfile(args.manifest):
        print(
            f"Manifest file not found at location '{args.manifest}'. Exiting.",
            file=sys.stderr)
        return 1

    manifest = {}
    with open(args.manifest, 'r') as manifest_file:
        manifest = json.load(manifest_file)

    paths_and_merkles = [
        (blob['path'], blob['merkle']) for blob in manifest['blobs']
    ]

    generated_package_api = {}
    generated_package_api['version'] = manifest['version']
    # OrderedDict used to keep files in sorted order, while keeping top-level
    # values in declared order.
    generated_package_api['content'] = {'files': collections.OrderedDict()}
    for path, merkle in paths_and_merkles:
        # Ensure no duplicate files.
        if path in generated_package_api['content']['files']:
            print(
                f"Path found multiple times in manifest: '{path}'",
                file=sys.stderr)
            return 1

        if path not in args.expected_files_exact:
            # Add as an 'internal' file.
            generated_package_api['content']['files'][path] = {"internal": True}
        else:
            # Add as an 'exact' file.
            generated_package_api['content']['files'][path] = {"hash": merkle}

    # Ensure all expected 'internal' files seen in manifest.
    for expected_file_internal in args.expected_files_internal:
        if expected_file_internal not in generated_package_api['content'][
                'files'].keys():
            print(
                f"File declared internal not found manifest: '{expected_file_internal}'",
                file=sys.stderr)
            return 1

    generated_package_api_str = json.dumps(
        generated_package_api, indent=2)

    with open(args.output, 'w') as output_file:
        output_file.write(generated_package_api_str)

    # If present, ensure generated file matches golden.
    if args.reference is not None:
        passed_golden = False
        if not os.path.isfile(args.reference):
            print(
                f"Golden file specified, but no file found at {args.reference}.", file=sys.stderr)
        else:
            with open(args.reference, 'r') as manifest_file:
                golden = json.load(manifest_file)

            golden_str = json.dumps(golden, indent=2)

            if not generated_package_api_str == golden_str:
                print(
                    "Golden and generated api file do not match.", file=sys.stderr)
                print(
                    "\n".join(
                        difflib.unified_diff(golden_str.splitlines(),
                                            generated_package_api_str.splitlines())),
                    file=sys.stderr)
            else:
                passed_golden = True

        if not passed_golden:
            print(
                "To overwrite the golden file location with generated api file, issue this command from your build directory:",
                file=sys.stderr)
            print(f"> 'cp {args.output} {args.reference}'", file=sys.stderr)
            if not args.warn:
                return 1

    return 0


if __name__ == '__main__':
    sys.exit(main())
