#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generates a content checklist file for inclusion in the IDK by reading the passed
package manifest, files (with specified dispositions), and reference to compare
with. For use in sdk_fuchsia_package() template."""

import argparse
import collections
import difflib
import json
import os
import subprocess
import sys
import tempfile


def get_meta_far_contents(ffx_bin, far_bin, meta_far_source_path):
    """
    Takes a path to a meta far file, and returns a list
    of tuples of form: (file_path, content_hash)
    """
    # Extract file paths from meta.far
    meta_far_paths_and_merkles = []
    meta_far_list_result = subprocess.run(
        [far_bin, 'list', f"--archive={meta_far_source_path}"],
        stdout=subprocess.PIPE,
        text=True)
    meta_far_file_paths = meta_far_list_result.stdout.split('\n')

    # Extract contents of file paths, and calculate content hash
    for file_path in sorted(meta_far_file_paths):
        if file_path == "":
            continue

        content = subprocess.run(
            [ffx_bin, 'package', 'far', 'cat', meta_far_source_path, file_path],
            stdout=subprocess.PIPE)

        with tempfile.NamedTemporaryFile("wb") as temp_file:
            temp_file.write(content.stdout)
            temp_file.flush()
            file_hash = subprocess.run(
                [ffx_bin, 'package', 'file-hash', temp_file.name],
                stdout=subprocess.PIPE,
                text=True)

            file_content_hash = file_hash.stdout.split()[0]
            meta_far_paths_and_merkles.append((file_path, file_content_hash))

    return meta_far_paths_and_merkles


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        '--manifest', help='Path to the package manifest.', required=True)
    parser.add_argument(
        '--output',
        help='Path to the content checklist file to compute.',
        required=True)
    parser.add_argument(
        '--ffx-bin', help='Path to ffx tooling.', required=False)
    parser.add_argument(
        '--far-bin', help='Path to far tooling.', required=False)
    parser.add_argument(
        '--expected-files-exact',
        action='append',
        help='Exact files to capture.',
        required=False,
        default=[])
    parser.add_argument(
        '--expected-files-present',
        action='append',
        help='Present files to capture.',
        required=False,
        default=[])
    parser.add_argument(
        '--reference',
        help='Path to the golden content checklist file',
        required=False)
    parser.add_argument(
        '--warn',
        help='Whether content checklist changes should only cause warnings',
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

    # Retrieve paths and merkles from `meta/` entities.
    if args.ffx_bin and args.far_bin:
        for blob in manifest['blobs']:
            if blob['path'] == 'meta/':
                meta_far_source_path = blob['source_path']
                meta_far_paths_and_merkles = get_meta_far_contents(
                    args.ffx_bin, args.far_bin, meta_far_source_path)
                paths_and_merkles += meta_far_paths_and_merkles

                break

    generated_package_content_checklist = {}
    generated_package_content_checklist['version'] = manifest['version']
    # OrderedDict used to keep files in sorted order, while keeping top-level
    # values in declared order.
    generated_package_content_checklist['content'] = {
        'files': collections.OrderedDict()
    }
    for path, merkle in paths_and_merkles:
        # Ensure no duplicate files.
        if path in generated_package_content_checklist['content']['files']:
            print(
                f"Path found multiple times in manifest: '{path}'",
                file=sys.stderr)
            return 1

        if path in args.expected_files_present:
            # Add as a 'present' file.
            generated_package_content_checklist['content']['files'][path] = {
                "present": True
            }
        if path in args.expected_files_exact:
            # Add as an 'exact' file.
            generated_package_content_checklist['content']['files'][path] = {
                "hash": merkle
            }

    # Ensure all expected 'present' files seen in manifest.
    for expected_file_present in args.expected_files_present:
        if expected_file_present not in generated_package_content_checklist[
                'content']['files'].keys():
            print(
                f"File declared 'present' not found manifest: '{expected_file_present}'. Files available from manifest and meta far are:",
                file=sys.stderr)
            print(
                "\n".join(sorted([path for path, _ in paths_and_merkles])),
                file=sys.stderr)
            return 1

    generated_package_content_checklist_str = json.dumps(
        generated_package_content_checklist, indent=2)

    with open(args.output, 'w') as output_file:
        output_file.write(generated_package_content_checklist_str)

    # If present, ensure generated file matches golden.
    if args.reference is not None:
        passed_golden = False
        if not os.path.isfile(args.reference):
            print(
                f"Golden file specified, but no file found at {args.reference}.",
                file=sys.stderr)
        else:
            with open(args.reference, 'r') as manifest_file:
                golden = json.load(manifest_file)

            golden_str = json.dumps(golden, indent=2)

            if not generated_package_content_checklist_str == golden_str:
                print(
                    "Error: SDK package golden and generated content checklist file do not match.",
                    file=sys.stderr)
                print(
                    "\n".join(
                        difflib.unified_diff(
                            golden_str.splitlines(),
                            generated_package_content_checklist_str.splitlines(
                            ))),
                    file=sys.stderr)
            else:
                passed_golden = True

        if not passed_golden:
            print(
                "To overwrite the golden file location with the newly generated content checklist file, issue this command:",
                file=sys.stderr)
            print(
                f'  mkdir -p "{os.path.dirname(args.reference)}" && cp {os.path.abspath(args.output)} {args.reference}',
                file=sys.stderr)
            if not args.warn:
                return 1

    return 0


if __name__ == '__main__':
    sys.exit(main())
