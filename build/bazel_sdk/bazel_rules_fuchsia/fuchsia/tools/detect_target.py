#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import subprocess
import sys

from pathlib import Path
from fuchsia_task_lib import Terminal

_PRODUCT_BUNDLE_REPO_ALIASES = ['fuchsia.com', 'chromium.org']

def run_json(*command):
    return json.loads(run(*command))


def run(*command):
    try:
        return subprocess.check_output(
            command,
            text=True,
        ).strip()
    except subprocess.CalledProcessError as e:
        print(e.stdout)
        raise e


class Target:
    def __init__(self, json):
        def child_value(e, name):
            for c in entry['child']:
                if c['label'] == name:
                    return c['value']
            return None

        for entry in json:
            label = entry['label']
            if label == 'target':
                self.name = child_value(entry, 'name')
            elif label == 'build':
                self.product = child_value(entry, 'product')
                self.board = child_value(entry, 'board')
                self.version = child_value(entry, 'version')


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
        '--product_bundle',
        type=str,
        help='The name of the product bunde (<product>.<board>).',
        required=True,
    )

    parser.add_argument(
        '--product_bundle_repo',
        type=str,
        help='The name of the product bunde repository hosting the product bundle.',
        required=True,
    )

    parser.add_argument(
        '--package_repo',
        type=str,
        help='The name of the package repo to register with the target.',
        required=False,
    )

    return parser.parse_args()


def all_targets(args):
    try:
        target_list_result = run_json(args.ffx, '--machine', 'json', 'target', 'list')
    except:
        return []

    nodes = [t['nodename'] for t in target_list_result]
    targets = []
    for node in nodes:
        node_json = run_json(args.ffx, '--target', node, 'target', 'show', '--json')
        targets.append(Target(node_json))

    return targets


def get_product_and_board(args):
    return args.product_bundle.split(".")

def filter_targets(args, targets):
    (product, board) = get_product_and_board(args)
    filtered = []
    for target in targets:
        if target.product == product and target.board == board:
            filtered.append(target)

    return filtered


def make_target_default(args, target):
    green_name = Terminal.green(target.name)
    print('Setting {} as default target'.format(green_name))
    run(args.ffx, 'target', 'default', 'set', target.name)


def register_repo_with_target(args, target, repo, aliases = []):
    cmd = [args.ffx,
        "target",
        "repository",
        "register",
        "-r",
        repo,
    ]
    for alias in aliases:
        cmd.extend(["--alias", alias])

    run(*cmd)


def detect_target(args, targets):
    assert(len(targets) > 0)

    if len(targets) > 1:
        # TODO: Make it so users can select their target here
        print(f'{Terminal.red("FAIL")} - multiple targets found cannot set default"')
    else:
        target = targets[0]
        if args.product_bundle_repo:
            register_repo_with_target(args, target, args.product_bundle_repo, _PRODUCT_BUNDLE_REPO_ALIASES)
        if args.package_repo:
            register_repo_with_target(args, target, args.package_repo)

        make_target_default(args, target)


def notify_no_targets_found(args, known_targets):
    print(f'{Terminal.red("ERROR: No targets found running {}".format(args.product_bundle))}')
    if known_targets:
        print('The following targets were found:')
        for target in known_targets:
            print(' - {} ({}.{})'.format(target.name, target.product, target.board))
    sys.exit(1)


def main():
    args = parse_args()
    known_targets = all_targets(args)
    matching_targets = filter_targets(args, known_targets)

    if matching_targets:
        detect_target(args, matching_targets)
    else:
        notify_no_targets_found(args, known_targets)


if __name__ == "__main__":
    main()
