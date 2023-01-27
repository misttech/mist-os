#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import subprocess
import os

from pathlib import Path
from shutil import rmtree
from fuchsia_task_lib import Terminal

def run(*command):
    try:
        return subprocess.check_output(
            command,
            text=True,
        ).strip()
    except subprocess.CalledProcessError as e:
        print(e.stdout)
        raise e

def parse_args():
    '''Parses arguments.'''
    parser = argparse.ArgumentParser()

    def path_arg(type='file'):
        def arg(path):
            path = Path(path)
            if path.is_file() != (type == 'file') or path.is_dir() != (type == 'directory'):
                parser.error(f'Path "{path}" is not a {type}!')
            return path
        return arg


    parser.add_argument(
        '--ffx',
        type=path_arg(),
        help='A path to the ffx tool.',
        required=True,
    )
    parser.add_argument(
        '--name',
        type=str,
        help='The name of the repository to clean',
        required=True,
    )
    parser.add_argument(
        '--fallback_path',
        type=str,
        help='The path that the user thinks should hold the package.',
        required=False,
    )

    parser.add_argument(
        '--delete_contents',
        help='If True, the on-disk contents will be deleted',
        action='store_true'
    )
    parser.add_argument(
        '--no-delete_contents',
        help='If True, the on-disk contents will be deleted',
        dest='delete_contents',
        action='store_false'
    )
    parser.set_defaults(delete_contents=True)

    return parser.parse_args()

def repo_path(args):
    '''Checks if the repo exists and returns the path'''
    repos = json.loads(run(
        args.ffx,
        '--machine',
        'JSON', 'repository',
        'list'
    ))
    for repo in repos:
        if repo['name'] == args.name:
            return repo['spec']['path']

    return None

def rm_repo(args, path):
    '''Removes the repo from ffx and on disk'''
    run(args.ffx, 'repository', 'remove', args.name)
    try:
        if args.delete_contents:
            rmtree(path)
    except:
        print("Unable to remove package repository '{}' at {}".format(args.name, path))
        print("This package was likely removed by another process and not removed from ffx.")

    return None


def prompt_for_deleting_repo(args):
    print(f'{Terminal.red("WARNING:")} package repository {args.name} is not registered with ffx.')
    print('This likely means it was removed by another process and will need to be manually removed.')
    if not args.fallback_path:
        return

    if os.path.isabs(args.fallback_path):
        path = args.fallback_path
    else:
        path = os.path.join(os.environ['BUILD_WORKSPACE_DIRECTORY'], args.fallback_path)

    # Check if the path looks like a package repo
    try:
        contents = os.listdir(path)
        expected_contents =  ['staged', 'repository', 'keys']
        looks_like_repo = all(items in contents for items in expected_contents)
    except:
        looks_like_repo = False

    if looks_like_repo:
        print(f'Attempting to delete {Terminal.underline(path)}')
        should_delete = input('Would you like to proceed? (y/n): ').lower()
        if should_delete == 'y':
            rmtree(path)


def main():
    # Parse arguments.
    args = parse_args()
    path = repo_path(args)
    if path:
        rm_repo(args, path)
    else:
        prompt_for_deleting_repo(args)


if __name__ == '__main__':
    main()
