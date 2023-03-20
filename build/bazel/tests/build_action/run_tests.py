#!/usr/bin/env python3
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Run all the tests in this directory through `fx build`, and compare their
outputs, including the generated depfiles."""

import argparse
import filecmp
import os
import shlex
import shutil
import stat
import subprocess
import sys


def find_fuchsia_dir(from_path=os.getcwd()):
    """Locate Fuchsia source directory."""
    path = os.path.abspath(from_path)
    while True:
        if os.path.exists(os.path.join(path, '.jiri_manifest')):
            return path

        new_path = os.path.dirname(path)
        assert new_path != path, f'Could not find Fuchsia directory from {from_path}'
        path = new_path


def find_ninja_build_dir(fuchsia_dir):
    fx_build_dir_file = os.path.join(fuchsia_dir, '.fx-build-dir')
    if not os.path.exists(fx_build_dir_file):
        build_dir = 'out/default'
    else:
        with open(fx_build_dir_file) as f:
            build_dir = f.read().strip()

        assert len(
            build_dir) > 0, f'Empty Ninja build directory: {fx_build_dir_file}'

    return os.path.relpath(os.path.join(fuchsia_dir, build_dir), os.getcwd())


def compare_directories(expected_dir, actual_dir):
    success = True
    dcmp = filecmp.dircmp(expected_dir, actual_dir)
    if dcmp.left_only:
        print(f'ERROR: Missing files from {actual_dir}:', file=sys.stderr)
        for f in dcmp.left_only:
            print(f'  - {f}', file=sys.stderr)
        success = False

    if dcmp.right_only:
        print(
            f'ERROR: Extraneous files found in {actual_dir}:', file=sys.stderr)
        for f in dcmp.right_only:
            print(f'  - {f}', file=sys.stderr)
        success = False

    if dcmp.diff_files:
        print(
            f'ERROR: File differences found in {actual_dir}:', file=sys.stderr)
        for f in dcmp.diff_files:
            print(f'  - {f}', file=sys.stderr)
        success = False

    if dcmp.funny_files:
        print(
            f'ERROR: Uncomparable files found in {actual_dir}:',
            file=sys.stderr)
        for f in dcmp.funny_files:
            print(f'  - {f}', file=sys.stderr)
        success = False

    # Verify that the files in |actual_dir| are not read-only.
    # See https://fxbug.dev/121003
    readonly_files = []
    for out_file in dcmp.right_list:
        out_path = os.path.join(actual_dir, out_file)
        info = os.stat(out_path)
        if info.st_mode & stat.S_IWUSR == 0:
            readonly_files.append(out_file)
    if readonly_files:
        print(f'ERROR: Read-only files found in {actual_dir}:', file=sys.stderr)
        for f in readonly_files:
            print(f'  - {f}', file=sys.stderr)
        success = False

    return success


def find_tests(root_test_dir):
    result = []
    for entry in sorted(os.listdir(root_test_dir)):
        build_file = os.path.join(root_test_dir, entry, 'BUILD.gn')
        if not os.path.exists(build_file):
            continue

        # If the BUILD.gn contains a "test_must_fail" target definition,
        # verify later that it actually fails.
        test_must_fail = False
        with open(build_file) as f:
            if '"test_must_fail"' in f.read():
                test_must_fail = True

        result.append((entry, test_must_fail))

    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        '--fuchsia-dir', help='Specify Fuchsia source root directory.')
    parser.add_argument('--build_dir', help='Specify Ninja build directory.')

    args = parser.parse_args()
    if args.fuchsia_dir:
        fuchsia_dir = os.path.abspath(args.fuchsia_dir)
    else:
        fuchsia_dir = find_fuchsia_dir()

    root_test_dir = os.path.relpath(os.path.dirname(__file__), fuchsia_dir)

    build_dir = args.build_dir
    if build_dir is None:
        build_dir = find_ninja_build_dir(fuchsia_dir)

    print(
        f'''
Fuchsia dir:      {fuchsia_dir}
Root test dir:    {root_test_dir}
Ninja output dir: {build_dir}
''')

    def run_fx_command(cmd_args):
        cmd_args = ['scripts/fx'] + cmd_args
        cmd_str = ' '.join(shlex.quote(c) for c in cmd_args)
        ret = subprocess.run(
            cmd_args, capture_output=True, text=True, cwd=fuchsia_dir)
        return ret, cmd_str

    # Clear the tests' Ninja output directories
    root_test_ninja_gen_dir = os.path.join(build_dir, 'gen', root_test_dir)
    if os.path.exists(root_test_ninja_gen_dir):
        shutil.rmtree(root_test_ninja_gen_dir)

    tests = find_tests(root_test_dir)
    assert len(tests) > 0, f'Cannot find tests in {root_test_dir}'

    # Build all tests with Ninja
    # Note that `-d keepdepfile` is required to ensure that Ninja will not
    # remove the generated depfiles from the output directory (even though
    # they will still be injected into the .ninja_deps file for future
    # incremental builds).

    success = True

    for name, must_fail in tests:
        # Compare the directories.
        test_subdir = os.path.join(root_test_dir, name)
        test_gen_dir = os.path.join(root_test_ninja_gen_dir, name)
        test_expected_gen_dir = os.path.join(
            fuchsia_dir, test_subdir, 'expected.gen')

        target_name = 'test_must_fail' if must_fail else 'test'
        build_cmd = [
            'build',
            '-d',
            'keepdepfile',
            os.path.join(root_test_dir, name) + ':' + target_name,
        ]

        print(f'[RUNNING   ] {name}')
        ret, cmd_str = run_fx_command(build_cmd)
        cmd_success = True

        if must_fail:
            if ret.returncode == 0:
                print(f'UNEXPECTED_SUCCESS: {cmd_str}')
                cmd_success = False
            else:
                expected_error_file = os.path.join(
                    test_subdir, 'expected.error')
                with open(expected_error_file) as f:
                    expected_error = f.read().format(
                        ninja_build_dir=os.path.realpath(build_dir))

                # Ninja only reports stderr command errors through its own stdout!
                actual_error = ret.stdout
                if expected_error not in actual_error:
                    print('UNPEXECTED ERROR MESSAGE:')
                    for line in actual_error.splitlines():
                        print('   ' + line)
                    print('EXPECTED ERROR MESSAGE:')
                    for line in expected_error.splitlines():
                        print('   ' + line)
                    cmd_success = False
                else:
                    print('EXPECTED_FAILURE OK')
        elif ret.returncode != 0:
            print(
                'ERROR_STDOUT:\n{stdout}\nERROR_STDERR:\n{stderr}\n\nWhen invoking command: {cmd_str}'
                .format(stdout=ret.stdout, stderr=ret.stderr, cmd_str=cmd_str))
            cmd_success = False
        else:
            cmd_success = compare_directories(
                test_expected_gen_dir, test_gen_dir)

        if cmd_success:
            print(f'[   SUCCESS] {name}')
        else:
            print(f'[    FAILED] {name}')
            success = False

    if success:
        print('All good!')
        return 0

    return 1


if __name__ == "__main__":
    sys.exit(main())
