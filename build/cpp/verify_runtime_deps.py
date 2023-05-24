#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Verify the runtime dependencies of a prebuilt SDK library (static or shared).
See //build/cpp/verify_runtime_deps.gni for details. This takes two input files:

- A JSON file that lists the runtime dependencies of the library, each
  one of them through a schema described in //build/cpp/verify_runtime_deps.gni

- An SDK manifest file, another JSON file that describes the SDK atom for
  the target as well as _all_ its transitive dependencies. See sdk_atom()
  template for more details.

On success, a stamp file is written. On failure, error messages are printed
to stderr and the script returns with an error status.
"""

import argparse
import collections
import json
import os
import sys

# The following runtime libraries are provided directly by the SDK sysroot,
# and not as SDK atoms.
_SYSROOT_LIBS = ['libc.so', 'libzircon.so']


def parse_sdk_manifest(manifest):
    '''Parse SDK manifest file and extract atom id and dependencies.

    Args:
      manifest: A directionary representation of the JSON SDK manifest.

    Returns:
      an (sdk_id, deps) tuple, where 'sdk_id' is a string identifying the
      atom (e.g. 'sdk://pkg/async'), and 'deps' is a dictionary mapping
      the sdk ids of all transitive dependencies to the corresponding
      manifest JSON object.
    '''
    atom_id = manifest['ids'][0]

    def find_atom(id):
        return next(a for a in manifest['atoms'] if a['id'] == id)

    atom = find_atom(atom_id)
    deps = [find_atom(a) for a in atom['deps']]
    deps += [atom]

    # Maps sdk_ids to the corresponding entry
    deps_map = {dep['id']: dep for dep in deps}

    return atom_id, deps_map


_NON_SDK_DEPS_ERROR_HEADER = r'''## Non-SDK dependencies required at runtime:
'''

_NON_SDK_DEPS_ERROR_FOOTER = r'''
HINT: These should be defined by an sdk_shared_library() call, or by a
zx_library() one that sets 'sdk = "shared"'.
'''

_BAD_SDK_DEPS_ERROR_HEADER = r'''## Non prebuilt libraries required at runtime:
'''

_BAD_SDK_DEPS_ERROR_FOOTER = _NON_SDK_DEPS_ERROR_FOOTER

_MISSING_SDK_DEPS_ERROR_HEADER = r'''## No dependency generates SDK runtime requirement:
'''

_MISSING_SDK_DEPS_ERROR_FOOTER = r'''
HINT: Add the missing atom(s)'s targets to the 'runtime_deps' list.
'''


class DependencyErrors(object):
    """Models list of errors found during verification."""

    def __init__(self):
        self._errors = []

    def has_error(self):
        """Return True iff this instance contains errors."""
        return bool(self._errors)

    def add_non_sdk_dependency(self, entry):
        """Add an entry for a non-SDK dependency.

        This happens when an SDK atom depends on a shared_library() instance
        directly, instead of an skd_shared_library() one.
        """
        self._errors.append((entry, 'non_sdk_deps'))

    def add_bad_sdk_dependency(self, entry):
        """Add an entry for a non-prebuilt shared library."""
        self._errors.append((entry, 'bad_sdk_deps'))

    def add_missing_sdk_dependency(self, entry):
        """Add an entry for a non-SDK shared library dependency."""
        self._errors.append((entry, 'missing_sdk_deps'))

    def __str__(self):
        result = ''
        # Split errors by categories
        errors = collections.defaultdict(list)
        for entry, category in self._errors:
            errors[category].append(entry)

        for category, entries in errors.items():
            if category == 'non_sdk_deps':
                result += _NON_SDK_DEPS_ERROR_HEADER
                for entry in entries:
                    result += '- %s generated_by %s\n' % (
                        entry['source'], entry['label'])
                result += _NON_SDK_DEPS_ERROR_FOOTER

            elif category == 'bad_sdk_deps':
                result += _BAD_SDK_DEPS_ERROR_HEADER
                for entry in entries:
                    result += '- SDK_atom %s generated_by %s\n' % (
                        entry['sdk_id'], entry['label'])
                result += _BAD_SDK_DEPS_ERROR_FOOTER

            elif category == 'missing_sdk_deps':
                result += _MISSING_SDK_DEPS_ERROR_HEADER
                for entry in entries:
                    result += '- SDK_atom %s generated_by %s\n' % (
                        entry['sdk_id'], entry['label'])
                result += _MISSING_SDK_DEPS_ERROR_FOOTER

            else:
                assert False, 'Unknown category: ' % category

        return result


def check_missing_files(runtime_files, atom_deps):
    """Verify runtime requirements, and return a DependencyErrors instance."""
    errors = DependencyErrors()

    for entry in runtime_files:
        gn_label = entry['label']
        sdk_id = entry.get('sdk_id')
        source = entry.get('source')

        assert bool(sdk_id) != bool(source), (
            'Exactly one of "sdk_id" or "source" must be defined in entry: %s' %
            entry)

        if sdk_id:
            # This runtime dependency is an SDK library, verify that it is part
            # of the SDK manifest.
            dep = atom_deps.get(sdk_id)
            if not dep:
                errors.add_missing_sdk_dependency(entry)
            elif dep['type'] != 'cc_prebuilt_library':
                errors.add_bad_sdk_dependency(entry)
        elif source:
            # Ignore sysroot libs
            if os.path.basename(source) in _SYSROOT_LIBS:
                continue

            # This runtime dependency is *not* an SDK library, this is an error.
            errors.add_non_sdk_dependency(entry)

    return errors


def main():
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        '--sdk-runtime-deps',
        help='Path to the list of runtime deps.',
        required=True)
    parser.add_argument(
        '--sdk-manifest',
        help='Path to the target\'s SDK manifest file.',
        required=True)
    parser.add_argument(
        '--stamp-file', help='Path to the output stamp file.', required=True)
    args = parser.parse_args()

    with open(args.sdk_runtime_deps, 'r') as runtime_deps_file:
        runtime_files = json.load(runtime_deps_file)

    # Read the list of package dependencies for the library's SDK incarnation.
    with open(args.sdk_manifest, 'r') as manifest_file:
        manifest = json.load(manifest_file)

    atom_id, deps = parse_sdk_manifest(manifest)

    # Find atom label with `_sdk_manifest($toolchain_suffix)` removed
    atom_label, _, _ = deps[atom_id]['gn-label'].rpartition('_sdk_manifest(')

    # Check whether all runtime files are available for packaging.
    errors = check_missing_files(runtime_files, deps)
    if errors.has_error():
        print(
            r'''
ERROR: When verifying runtime dependencies for the SDK atom: %s generated_by %s

%s''' % (atom_id, atom_label, errors),
            file=sys.stderr)
        return 1

    with open(args.stamp_file, 'w') as stamp:
        stamp.write('Success!')

    return 0


if __name__ == '__main__':
    sys.exit(main())
