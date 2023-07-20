#!/usr/bin/env fuchsia-vendored-python
# Copyright 2018 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Merge the content of two SDKs into a new one.

Use either --output-archive or --output-directory to indicate the output.
Note that it is possible to use both at the same time.

To specify inputs, there are two ways due to compatibility:

The legacy way (only supports two inputs):

  * Use either --first-archive or --first-directory to indicate the first input.
  * Use either --second-archive or --second-directory to indicate the second input.

The new way:

  * Use either --input-archive or --input-directory to indicate an input.
  * as many times as necessary, which means at least twice, since there is no
    point in merging a single input.

It is possible to use a single input and a single output, in particular to
create an archive from a single input directory, without any merge operation
as in:

  merge.py --input-directory DIR --output-archive ARCHIVE

Another use case if filtering any stale artifacts from an input directory
since the merge/copy will only include files listed from in the SDK metadata file,
for example:

  INPUT_DIR=out/default/sdk/exported/core
  touch $INPUT_DIR/UNWANTED_FILE
  merge.py --input-directory $INPUT_DIR --output-directory /tmp/FOO

Will not create /tmp/FOO/UNWANTED_FILE since it is not listed in the SDK manifest.
"""

# See https://stackoverflow.com/questions/33533148/how-do-i-type-hint-a-method-with-the-type-of-the-enclosing-class
from __future__ import annotations

import argparse
import collections
import contextlib
import errno
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile

from operator import itemgetter
from functools import total_ordering
from typing import Any, Dict, Optional, Sequence, Set, Tuple

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
FUCHSIA_ROOT = os.path.dirname(  # $root
    os.path.dirname(  # scripts
        os.path.dirname(  # sdk
            SCRIPT_DIR)))  # merger
PIGZ_PATH = os.path.join(
    FUCHSIA_ROOT, "prebuilt", "third_party", "pigz", "pigz")

# Reminder about the layout of each SDK directory:
#
# The top-level meta/manifest.json describes the whole SDK content
# as a JSON object, whose 'parts' key is an array of objects with
# with the following schema:
#
#    "meta": [string]: Path to an SDK element metadata file, relative
#                      to the SDK directory.
#
#    "type": [string]: Element type.
#
# A few examples:
#
#    {
#      "meta": "docs/low_level.json",
#      "type": "documentation"
#    },
#    {
#      "meta": "fidl/fuchsia.accessibility.gesture/meta.json",
#      "type": "fidl_library"
#    },
#    {
#      "meta": "pkg/vulkan/vulkan.json",
#      "type": "data"
#    },
#
# Then each element metadata JSON file itself is a JSON object whose
# schema depend on its "type" value.
#

# TODO: Use typing.TypeAlias introduced in Python 3.10 when possible.
TypeAlias = Any

# An SdkManifest is just a JSON object implemented as a Python dictionary
# for now. Define an alias for type checking only.
SdkManifest: TypeAlias = Dict[str, Any]

Path: TypeAlias = str


@total_ordering
class Part(object):
    '''Models a 'parts' array entry from an SDK manifest.'''

    def __init__(self, json: Dict):
        self.meta = json['meta']
        self.type = json['type']

    def __lt__(self, other):
        return (self.meta, self.type) < (other.meta, other.type)

    def __eq__(self, other):
        assert isinstance(other, self.__class__)
        return self.meta == other.meta and self.type == other.type

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        return hash((self.meta, self.type))


def _ensure_directory(path: Path):
    '''Ensures that the directory hierarchy of the given path exists.'''
    os.makedirs(os.path.dirname(path), exist_ok=True)


class ElementMeta(object):
    '''Models the metadata of a given SDK element.'''

    def __init__(self, meta: Dict[str, Any]):
        self._meta = meta

    @property
    def type(self):
        '''Returns the SDK element type.'''
        if 'schema_id' in self._meta:
            return self._meta['data']['type']
        return self._meta['type']

    @property
    def json(self):
        '''Return the JSON object for this element metadata instance.'''
        return self._meta

    def get_files(self) -> Tuple[Set[Path], Dict[str, Set[Path]]]:
        '''Extracts the files associated with the given element.
        Returns a 2-tuple containing:
         - the set of arch-independent files;
         - the sets of arch-dependent files, indexed by architecture.
        '''
        type = self.type
        common_files = set()
        arch_files = {}
        if type == 'cc_prebuilt_library':
            common_files.update(self._meta['headers'])
            if 'ifs' in self._meta:
                common_files.add(
                    os.path.join(self._meta['root'], self._meta['ifs']))
            for arch, binaries in self._meta['binaries'].items():
                contents = set()
                contents.add(binaries['link'])
                if 'dist' in binaries:
                    contents.add(binaries['dist'])
                if 'debug' in binaries:
                    contents.add(binaries['debug'])
                arch_files[arch] = contents
        elif type == 'cc_source_library':
            common_files.update(self._meta['headers'])
            common_files.update(self._meta['sources'])
        elif type == 'dart_library':
            common_files.update(self._meta['sources'])
        elif type == 'ffx_tool':
            if 'files' in self._meta:
                for (name, collection) in self._meta["files"].items():
                    if name == "executable" or name == "executable_metadata":
                        common_files.add(collection)
                    else:
                        common_files.update(collection)
            if 'target_files' in self._meta:
                for (arch, binaries) in self._meta["target_files"].items():
                    arch_files[arch] = set()
                    for (name, collection) in binaries.items():
                        if name == "executable" or name == "executable_metadata":
                            arch_files[arch].add(collection)
                        else:
                            arch_files[arch].update(collection)
        elif type == 'fidl_library':
            common_files.update(self._meta['sources'])
        elif type in ['host_tool', 'companion_host_tool', 'package']:
            if 'files' in self._meta:
                common_files.update(self._meta['files'])
            if 'target_files' in self._meta:
                arch_files.update(self._meta['target_files'])
        elif type == 'loadable_module':
            common_files.update(self._meta['resources'])
            arch_files.update(self._meta['binaries'])
        elif type == 'sysroot':
            for ifs_file in self._meta['ifs_files']:
                common_files.add(os.path.join("pkg", "sysroot", ifs_file))
            for arch, version in self._meta['versions'].items():
                contents = set()
                contents.update(version['headers'])
                contents.update(version['link_libs'])
                contents.update(version['dist_libs'])
                contents.update(version['debug_libs'])
                arch_files[arch] = contents
        elif type == 'documentation':
            common_files.update(self._meta['docs'])
        elif type in ('config', 'license', 'component_manifest'):
            common_files.update(self._meta['data'])
        elif type in ('version_history'):
            # These types are pure metadata.
            pass
        elif type == 'bind_library':
            common_files.update(self._meta['sources'])
        else:
            raise Exception('Unknown element type: ' + type)

        return (common_files, arch_files)

    def merge_with(self, other: ElementMeta) -> ElementMeta:
        '''Merge current instance with another one and return new value.'''
        meta_one = self._meta
        meta_two = other._meta

        # TODO(fxbug.dev/5362): verify that the common parts of the metadata files are in
        # fact identical.
        type = self.type
        if type != other.type:
            raise Exception(
                'Incompatible element types (%s vs %s)' % (type, other.type))

        meta = {}
        if type in ('cc_prebuilt_library', 'loadable_module'):
            meta = meta_one
            meta['binaries'].update(meta_two['binaries'])
        elif type == 'package':
            meta = meta_one
            meta['package_manifests'] += meta_two['package_manifests']
            # Remove duplicate items, and sort final result.
            meta['package_manifests'] = sorted(
                [
                    dict(t) for t in
                    {tuple(d.items()) for d in meta['package_manifests']}
                ],
                key=itemgetter(
                    'api_level', 'target_architecture', 'manifest_file'))
            meta['target_files'].update(meta_two['target_files'])
        elif type == 'sysroot':
            meta = meta_one
            meta['versions'].update(meta_two['versions'])
        elif type in ['ffx_tool', 'host_tool', 'companion_host_tool']:
            meta = meta_one
            if not 'target_files' in meta:
                meta['target_files'] = {}
            if 'target_files' in meta_two:
                meta['target_files'].update(meta_two['target_files'])
        elif type in ('cc_source_library', 'dart_library', 'fidl_library',
                      'documentation', 'device_profile', 'config', 'license',
                      'component_manifest', 'bind_library', 'version_history'):
            # These elements are arch-independent, the metadata does not need any
            # update.
            meta = meta_one
        else:
            raise Exception('Unknown element type: ' + type)

        return ElementMeta(meta)


def _has_host_content(parts: Set[Part]):
    '''Returns true if the given list of SDK parts contains an element with
    content built for a host.
    '''
    return 'host_tool' in [part.type for part in parts]


def _merge_sdk_manifests(manifest_one: SdkManifest,
                         manifest_two: SdkManifest) -> Optional[SdkManifest]:
    """Merge two SDK manifests into one. Returns None in case of error."""
    parts_one = set([Part(p) for p in manifest_one['parts']])
    parts_two = set([Part(p) for p in manifest_two['parts']])

    manifest: SdkManifest = {'arch': {}}

    # Schema version.
    if manifest_one['schema_version'] != manifest_two['schema_version']:
        print('Error: mismatching schema version')
        return None
    manifest['schema_version'] = manifest_one['schema_version']

    # Host architecture.
    host_archs = set()
    if _has_host_content(parts_one):
        host_archs.add(manifest_one['arch']['host'])
    if _has_host_content(parts_two):
        host_archs.add(manifest_two['arch']['host'])
    if not host_archs:
        # The archives do not have any host content. The architecture is not
        # meaningful in that case but is still needed: just pick one.
        host_archs.add(manifest_one['arch']['host'])
    if len(host_archs) != 1:
        print(
            'Error: mismatching host architecture: %s' % ', '.join(host_archs))
        return None
    manifest['arch']['host'] = list(host_archs)[0]

    def compare_then_copy(name: str) -> bool:
        """Copy the value of a manifest field after checking for correctness."""
        value1 = manifest_one[name]
        value2 = manifest_two[name]
        if value1 != value2:
            print(f'Error: mismatching {name}: \'{value1}\' vs \'{value2}\'')
            return False

        manifest[name] = value1
        return True

    # Id.
    if not compare_then_copy('id'):
        return None

    # Root.
    if not compare_then_copy('root'):
        return None

    # Target architectures.
    manifest['arch']['target'] = sorted(
        set(manifest_one['arch']['target']) |
        set(manifest_two['arch']['target']))

    # Parts.
    manifest['parts'] = [vars(p) for p in sorted(parts_one | parts_two)]
    return manifest


def tarfile_writer(
        archive_file: Path, source_dir: Path, compressed: bool = True):
    """Write an archive using the Python tarfile module."""
    all_files: List[Tuple[str, str]] = []
    for root, dirs, files in os.walk(source_dir):
        for f in files:
            src_path = os.path.join(root, f)
            dst_path = os.path.relpath(src_path, source_dir)
            all_files.append((dst_path, src_path))
    all_files = sorted(all_files)

    options = "w:gz" if compressed else "w"
    with tarfile.open(archive_file, options) as archive:
        for dst_path, src_path in all_files:
            info = tarfile.TarInfo(dst_path)
            s = os.stat(src_path)
            info.size = s.st_size
            info.mode = s.st_mode
            # Leave all other fields as default for
            # deterministic output. uid/gid will be 0.
            # mtime will be 0 too (January 1st 1970), and type will be
            # tarfile.REGTYPE which is fine since directories or symlinks
            # are never stored in the archive.
            with open(src_path, 'rb') as f:
                archive.addfile(info, f)


def pigz_writer(archive_file: Path, source_dir: Path):
    """Write an uncompressed archive using the Python tarfile module,
    then compress with pigz."""
    temp_archive = archive_file + ".tmp.tar"
    tarfile_writer(temp_archive, source_dir, compressed=False)
    # Invoke pigz with its default block size of 128 KiB
    # The -n option avoids writing the name of the input archive into the result.
    # The -9 option enables max compression.
    subprocess.check_call([PIGZ_PATH, "-n", "-9", temp_archive])
    os.rename(temp_archive + ".gz", archive_file)


class MergeState(object):
    """Common state for all merge operations. Can be used as a context manager."""

    def __init__(self):
        self._all_outputs: Set[Path] = set()
        self._all_inputs: Set[Path] = set()
        self._temp_dirs: Set[Path] = set()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, exc_tb):
        self._remove_all_temp_dirs()
        return False  # Do not suppress exceptions

    def get_temp_dir(self) -> Path:
        """Return new temporary directory path."""
        temp_dir = tempfile.mkdtemp(prefix='fuchsia-sdk-merger')

        self._temp_dirs.add(temp_dir)
        return temp_dir

    def _remove_all_temp_dirs(self):
        """Remove all temporary directories."""
        for temp_dir in self._temp_dirs:
            shutil.rmtree(temp_dir, ignore_errors=True)

    def _is_temp_file(self, path: Path) -> bool:
        assert os.path.isabs(path)
        for tmp_dir in self._temp_dirs:
            if os.path.commonprefix([path, tmp_dir]) == tmp_dir:
                return True
        return False

    def add_output(self, path: Path):
        """Add an output path, ignored if temporary."""
        # For outputs, do not follow symlinks.
        path = os.path.abspath(path)
        if not self._is_temp_file(path):
            self._all_outputs.add(path)

    def add_input(self, path: Path):
        """Add an input path, ignored if temporary."""
        # For inputs, always resolve symlinks since that what matters
        # for Ninja (which never follows symlinks themselves).
        path = os.path.abspath(os.path.realpath(path))
        if not self._is_temp_file(path):
            self._all_inputs.add(path)

    def get_depfile_inputs_and_outputs(
            self) -> Tuple[Sequence[Path], Sequence[Path]]:
        """Return the lists of inputs and outputs that should appear in the depfile."""

        def make_relative_paths(paths):
            return sorted([os.path.relpath(os.path.realpath(p)) for p in paths])

        return make_relative_paths(self._all_inputs), make_relative_paths(
            self._all_outputs)

    def open_archive(self, archive: Path) -> Path:
        """Uncompress an archive and return the path of its temporary extraction directory."""
        extract_dir = self.get_temp_dir()
        with tarfile.open(archive) as archive_file:
            archive_file.extractall(extract_dir)
        return extract_dir

    def write_archive(self, archive: Path, source_dir: Path):
        """Write a compressed archive, using the pigz prebuilt if available"""
        if os.path.isfile(PIGZ_PATH):
            pigz_writer(archive, source_dir)
        else:
            tarfile_writer(archive, source_dir)
        self.add_output(archive)

    def write_json_output(self, path: Path, content: Any, dry_run: bool):
        """Write JSON output file."""
        if not dry_run:
            _ensure_directory(path)
            with open(path, 'w') as f:
                json.dump(
                    content, f, indent=2, sort_keys=True, separators=(',', ':'))
        self.add_output(path)


class InputSdk(object):
    """Models a single input SDK archive or directory during merge operations."""

    def __init__(self, archive: Path, directory: Path, state: MergeState):
        """Initialize instance. Either archive or directory must be set."""
        self._state = state
        if archive:
            assert not directory, 'Cannot set both archive and directory'
            self._directory = state.open_archive(archive)
        else:
            assert directory, 'Either archive or directory must be set'
            self._directory = directory

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, exc_tb):
        # Nothing to do here, extracted archive will be removed by
        # MergeState.__exit__() itself.
        return False

    @property
    def directory(self) -> Path:
        """Return directory path."""
        return self._directory

    def _read_json(self, file: Path) -> Any:
        source = os.path.join(self._directory, file)
        self._state.add_input(source)
        with open(source) as f:
            return json.load(f)

    def get_manifest(self) -> SdkManifest:
        """Return the manifest for this SDK."""
        return self._read_json(os.path.join('meta', 'manifest.json'))

    def get_element_meta(self, element: Path) -> ElementMeta:
        """Return the contents of the given element's manifest."""
        # 'element' is actually a path to a meta.json file, relative
        # to the SDK's top directory.
        return ElementMeta(self._read_json(element))


class OutputSdk(object):
    """Model either an output archive or directory during a merge operation."""

    def __init__(
            self, archive: Path, directory: Path, dry_run: bool,
            state: MergeState):
        """Initialize instance. Either archive or directory must be set."""
        self._dry_run = dry_run
        self._archive = archive
        self._state = state
        if directory:
            # NOTE: If both directory and archive are set, the directory
            # will be populated with symlinks, then the archive will be
            # created from its content in __exit__() below.
            if self._dry_run:
                # Use a temporary directory to keep the destination directory
                # untouched during dry-runs.
                self._directory = state.get_temp_dir()
            else:
                self._directory = directory
        elif archive:
            # If only archive is provided, create a temporary output directory
            # for its content.
            self._directory = state.get_temp_dir()
        else:
            assert False, 'Either archive or directory must be set'

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, exc_tb):
        if exc_type is None and self._archive:
            if self._dry_run:
                self._state.add_output(self._archive)
            else:
                self._state.write_archive(self._archive, self._directory)
        return False  # Do not supress exceptions.

    def write_manifest(self, manifest):
        self._state.write_json_output(
            os.path.join(self._directory, 'meta', 'manifest.json'), manifest,
            self._dry_run)

    def write_element_meta(self, element: Path, element_meta: ElementMeta):
        self._state.write_json_output(
            os.path.join(self._directory, element), element_meta.json,
            self._dry_run)

    def copy_file(self, file, source_dir):
        '''Copies a file to a given sub-path, taking care of creating directories if
       needed.
       '''
        source = os.path.join(source_dir, file)
        destination = os.path.join(self._directory, file)
        if not self._dry_run:
            _ensure_directory(destination)
            # shutil.copy2() will complain when copying a symlinks into the same symlink.
            if os.path.islink(destination):
                os.unlink(destination)
            shutil.copy2(source, destination, follow_symlinks=False)
        self._state.add_input(source)
        self._state.add_output(destination)

    def copy_files(self, files, source_dir):
        for file in files:
            self.copy_file(file, source_dir)

    def copy_identical_files(self, set_one, set_two, source_dir):
        if set_one != set_two:
            return False
        self.copy_files(set_one, source_dir)
        return True

    def copy_element(self, element: Path, source_sdk: InputSdk):
        '''Copy an entire SDK element to a given directory.'''
        meta = source_sdk.get_element_meta(element)
        assert meta is not None, 'Could not find metadata for element: %s, %s, %s' % (
            element, meta, source_sdk)
        common_files, arch_files = meta.get_files()
        files = common_files
        for more_files in arch_files.values():
            files.update(more_files)
        self.copy_files(files, source_sdk.directory)
        # Copy the metadata file as well.
        self.copy_file(element, source_sdk.directory)


def merge_sdks(
        first_sdk: InputSdk, second_sdk: InputSdk,
        output_sdk: OutputSdk) -> bool:
    first_manifest = first_sdk.get_manifest()
    second_manifest = second_sdk.get_manifest()
    first_parts = set([Part(p) for p in first_manifest['parts']])
    second_parts = set([Part(p) for p in second_manifest['parts']])
    common_parts = first_parts & second_parts

    # Copy elements that appear in a single SDK
    for element_part in sorted(first_parts - common_parts):
        output_sdk.copy_element(element_part.meta, first_sdk)
    for element_part in sorted(second_parts - common_parts):
        output_sdk.copy_element(element_part.meta, second_sdk)

    # Verify and merge elements which are common to both SDKs.
    for raw_part in sorted(common_parts):
        element = raw_part.meta
        first_meta = first_sdk.get_element_meta(element)
        second_meta = second_sdk.get_element_meta(element)
        first_common, first_arch = first_meta.get_files()
        second_common, second_arch = second_meta.get_files()

        # Common files should not vary.
        if not output_sdk.copy_identical_files(first_common, second_common,
                                               first_sdk.directory):
            print('Error: different common files for %s' % (element))
            return False

        # Arch-dependent files need to be merged in the metadata.
        all_arches = set(first_arch.keys()) | set(second_arch.keys())
        for arch in all_arches:
            if arch in first_arch and arch in second_arch:
                if not output_sdk.copy_identical_files(first_arch[arch],
                                                       second_arch[arch],
                                                       first_sdk.directory):
                    print('Error: different %s files for %s' % (arch, element))
                    return False
            elif arch in first_arch:
                output_sdk.copy_files(first_arch[arch], first_sdk.directory)
            elif arch in second_arch:
                output_sdk.copy_files(second_arch[arch], second_sdk.directory)

        new_meta = first_meta.merge_with(second_meta)
        output_sdk.write_element_meta(element, new_meta)

    output_manifest = _merge_sdk_manifests(first_manifest, second_manifest)
    if not output_manifest:
        return False

    output_sdk.write_manifest(output_manifest)
    return True


# A pair used to model either an input archive or an input directory.
InputInfo = collections.namedtuple('InputInfo', 'archive directory')


def make_archive_info(archive: Path) -> InputInfo:
    return InputInfo(archive=archive, directory=None)


def make_directory_info(directory: Path) -> InputInfo:
    return InputInfo(archive=None, directory=directory)


class InputAction(argparse.Action):
    """custom sub-class to handle input arguments.

    This works by storing in the 'inputs' namespace attribute a
    list of InputInfo values.
    """

    def __init__(self, option_strings, dest, **kwargs):
        dest = 'inputs'
        super(InputAction, self).__init__(option_strings, dest, **kwargs)

    def __call__(self, parser, namespace, values, option_string=None):
        assert isinstance(
            values, str), "Unsupported add_argument() 'type' value"
        if option_string == '--input-directory':
            input = make_directory_info(values)
        elif option_string == '--input-archive':
            input = make_archive_info(values)
        else:
            assert False, "Unsupported options string %s" % option_string

        inputs = getattr(namespace, self.dest)
        if inputs is None:
            inputs = []
        inputs.append(input)
        setattr(namespace, self.dest, inputs)


def main(main_args=None):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument(
        '--input-directory',
        help='Path to an input SDK - as a directory',
        metavar='DIR',
        action=InputAction,
    )
    parser.add_argument(
        '--input-archive',
        help='Path to an input SDK -as an archive',
        metavar='ARCHIVE',
        action=InputAction,
    )
    first_group = parser.add_mutually_exclusive_group()
    first_group.add_argument(
        '--first-archive',
        help='Path to the first SDK - as an archive',
        metavar='ARCHIVE1',
        default='')
    first_group.add_argument(
        '--first-directory',
        help='Path to the first SDK - as a directory',
        metavar='DIR1',
        default='')
    second_group = parser.add_mutually_exclusive_group()
    second_group.add_argument(
        '--second-archive',
        help='Path to the second SDK - as an archive',
        metavar='ARCHIVE2',
        default='')
    second_group.add_argument(
        '--second-directory',
        help='Path to the second SDK - as a directory',
        metavar='DIR2',
        default='')
    parser.add_argument(
        '--output-archive',
        help='Path to the merged SDK - as an archive',
        metavar='OUT_ARCHIVE',
        default='')
    parser.add_argument(
        '--output-directory',
        help='Path to the merged SDK - as a directory',
        metavar='OUT_DIR',
        default='')
    parser.add_argument('--stamp-file', help='Path to the stamp file')
    hermetic_group = parser.add_mutually_exclusive_group()
    hermetic_group.add_argument('--depfile', help='Path to the stamp file')
    hermetic_group.add_argument(
        '--hermetic-inputs-file', help='Path to the hermetic inputs file')
    args = parser.parse_args(main_args)

    # Convert --first-xx and --second-xxx options into the equivalent
    # --input-xxx ones.
    if args.first_archive or args.first_directory:
        if args.inputs:
            parser.error(
                'Cannot use --input-xxx option with --first-xxx option!')
            return 1

        if args.first_archive:
            first_input = make_archive_info(args.first_archive)
        else:
            first_input = make_directory_info(args.first_directory)

        if args.second_archive:
            second_input = make_archive_info(args.second_archive)
        elif args.second_directory:
            second_input = make_directory_info(args.second_directory)
        else:
            parser.error('Using --first-xxx requires --second-xxx too!')
            return 1

        args.inputs = [first_input, second_input]

    elif args.second_archive or args.second_directory:
        parser.error('Using --second-xxx requires --first-xxx too!')
        return 1

    if not args.inputs:
        parser.error(
            'At least one of --input-archive or --input directory is required!')

    if not args.output_archive and not args.output_directory:
        parser.error(
            'At least one of --output-archive or --output-directory is required!'
        )

    if len(args.inputs
          ) == 1 and args.inputs[0].archive and args.output_directory:
        parser.error(
            'Using a single input archive as input and an output directory is not supported!\n'
            +
            'as the result would contain dangling symlinks. Just uncompress the archive manually!'
        )

    has_hermetic_inputs_file = bool(args.hermetic_inputs_file)

    has_errors = False

    with MergeState() as state:
        num_inputs = len(args.inputs)
        for n, input in enumerate(args.inputs):
            input_sdk = InputSdk(
                args.inputs[n].archive, args.inputs[n].directory, state)

            if n == 0:
                previous_input_sdk = input_sdk
                if num_inputs > 1:
                    # Just record the first entry, no merge needed.
                    continue

            if n + 1 == num_inputs:
                # The final output directory or archive.
                out_archive = args.output_archive
                out_directory = args.output_directory
                out_dryrun = has_hermetic_inputs_file
            else:
                # This is an intermediate merge operation, use a temporary directory for it.
                out_archive = None
                out_directory = state.get_temp_dir()
                out_dryrun = False
            # Perform the merge operation
            with OutputSdk(out_archive, out_directory, out_dryrun,
                           state) as output_sdk:
                if not merge_sdks(previous_input_sdk, input_sdk, output_sdk):
                    return 1

            # Use intermediate output as the first input for the next operation.
            if n + 1 != num_inputs:
                previous_input_sdk = InputSdk(None, out_directory, state)

        depfile_inputs, depfile_outputs = state.get_depfile_inputs_and_outputs()
        if args.hermetic_inputs_file:
            with open(args.hermetic_inputs_file, 'w') as hermetic_inputs_file:
                hermetic_inputs_file.write('\n'.join(depfile_inputs))

        if args.depfile:
            with open(args.depfile, 'w') as depfile:
                depfile.write(
                    '{}: {}'.format(
                        ' '.join(depfile_outputs), ' '.join(depfile_inputs)))

    if args.stamp_file and not has_hermetic_inputs_file:
        with open(args.stamp_file, 'w') as stamp_file:
            stamp_file.write('')

    return 1 if has_errors else 0


if __name__ == '__main__':
    sys.exit(main())
