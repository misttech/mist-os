#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import subprocess
import shlex
import sys

from pathlib import Path
from typing import List

from terminal import Terminal

DESCRIPTION = '''
Publishes a given set of packages.

TODO(fxbug.dev/127875): Currently only cache packages are supported by this
tool.
'''


def run(*command: str, failure_message: str, **kwargs) -> None:
    command = shlex.join([str(part) for part in command])
    try:
        subprocess.check_call(command, shell=True, **kwargs)
    except subprocess.SubprocessError as e:
        Terminal.error(f'Command failed: {command}')
        Terminal.fatal(failure_message)


def fx_command(*command: str) -> List[str]:
    return [
        'bash',
        '-c',
        ' '.join(
            [
                '.',
                str(
                    (Path(__file__).parent.parent / 'lib' /
                     'vars.sh').resolve()), '&&', 'fx-config-read', '&&',
                'fx-command-run',
                shlex.join([str(part) for part in command])
            ]),
    ]


def main() -> int:
    parser = argparse.ArgumentParser(
        prog='fx publish',
        formatter_class=argparse.RawTextHelpFormatter,
        description=DESCRIPTION,
    )
    parser.add_argument(
        'packages',
        help=
        'Specify packages to publish. Currently, only cache packages are supported.',
        action='store',
        nargs='+',
    )
    parser.add_argument(
        '--repo',
        help=
        'Publish to this repo directory (relative to the build directory); defaults to `amber-files`.',
        default='amber-files',
    )
    parser.add_argument(
        '--no-build',
        dest='build',
        default=True,
        help='Publish only, skipping `fx build`.',
        action='store_false',
    )
    parser.add_argument(
        '--quiet',
        help='Suppress messages.',
        action='store_true',
    )
    parser.add_argument(
        '--stamp',
        help='Specify a stamp file to write to after publishing.',
    )
    args = parser.parse_args()

    Terminal.suppress = args.quiet

    build_dir = Path('.').resolve()

    package_tool = build_dir / 'host-tools' / 'package-tool'
    repo = build_dir / args.repo
    publish_tool_opts = (repo /
                         'publish_tool_opts').read_text().strip().split('\n')

    # Collect ninja targets.
    ninja_targets = ['build/images/updates:prepare_publish']
    packages_to_publish = []
    for packages in set(args.packages):
        if packages == 'cache':
            ninja_targets.append('assembly_cache_packages.list')
            packages_to_publish.append(
                build_dir / 'assembly_cache_packages.list')
        else:
            Terminal.fatal(f'Unrecognized packages "{packages}".')

    # Build step.
    if args.build:
        run(
            *fx_command('build', *ninja_targets),
            failure_message='Build failures!',
        )

    # Publish the packages.
    run(
        package_tool,
        'repository',
        'publish',
        repo,
        *publish_tool_opts,
        '--package-list',
        *packages_to_publish,
        failure_message='An internal error occured while publishing.',
    )

    # Report package publishing.
    publish_count = sum(
        [
            len(json.loads(file.read_text())['content']['manifests'])
            for file in packages_to_publish
        ])
    publish_count_msg = f'Published {publish_count} packages'
    if not Terminal.suppress:
        if subprocess.run(shlex.join(fx_command('is-package-server-running')),
                          shell=True, capture_output=True).returncode:
            Terminal.warn(
                f'{publish_count_msg}, but it looks like the package server is not running.'
            )
            Terminal.warn('You probably need to run "fx serve".')
        else:
            Terminal.info(f'{publish_count_msg}!')

    # Write stamp file.
    if args.stamp:
        Path(args.stamp).write_text(str(publish_count))


if __name__ == '__main__':
    sys.exit(main())
