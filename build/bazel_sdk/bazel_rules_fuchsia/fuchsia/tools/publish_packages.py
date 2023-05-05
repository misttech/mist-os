#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import subprocess
import tempfile

from pathlib import Path
from shutil import rmtree

from fuchsia_task_lib import *


def run(*command):
    try:
        return subprocess.check_output(
            command,
            text=True,
        ).strip()
    except subprocess.CalledProcessError as e:
        print(e.stdout)
        raise TaskExecutionException(f'Command {command} failed.')


class FuchsiaTaskPublish(FuchsiaTask):

    def parse_args(self, parser: ScopedArgumentParser) -> argparse.Namespace:
        '''Parses arguments.'''

        parser.add_argument(
            '--ffx',
            type=parser.path_arg(),
            help='A path to the ffx tool.',
            required=True,
        )
        parser.add_argument(
            '--pm',
            type=parser.path_arg(),
            help='A path to the pm tool.',
            required=True,
        )
        parser.add_argument(
            '--packages',
            help='Paths to far files to package.',
            nargs='+',
            type=parser.path_arg(),
            required=True,
        )
        parser.add_argument(
            '--repo_name',
            help='Optionally specify the repository name.',
            scope=ArgumentScope.GLOBAL,
        )
        parser.add_argument(
            '--target',
            help='Optionally specify the target fuchsia device.',
            required=False,
            scope=ArgumentScope.GLOBAL,
        )

        # Private arguments.
        # Whether to make the repo default.
        parser.add_argument(
            '--make_repo_default',
            action='store_true',
            help=argparse.SUPPRESS,
            required=False)
        # The effective repo path to publish to.
        parser.add_argument(
            '--repo_path',
            help=argparse.SUPPRESS,
            required=False,
        )
        return parser.parse_args()

    def enable_ffx_repository(self, args):
        if run(args.ffx, 'config', 'get',
               'repository.server.enabled') != 'true':
            print(
                'The ffx repository server is not enabled, starting it now...')
            run(args.ffx, 'repository', 'server', 'start')

    def ensure_target_device(self, args):
        args.target = args.target or run(args.ffx, 'target', 'default', 'get')
        print(f'Waiting for {args.target} to come online (60s)')
        run(args.ffx, '--target', args.target, 'target', 'wait', '-t', '60')

    def resolve_repo(self, args):
        # Determine the repo name we want to use, in this order:
        # 1. User specified argument.
        def user_specified_repo_name():
            if args.repo_name:
                print(f'Using manually specified --repo_name: {args.repo_name}')
                # We shouldn't ask to delete a user specified --repo_name.
                self.prompt_repo_cleanup = False
            return args.repo_name

        # 2. ffx default repository.
        def ffx_default_repo():
            default = run(
                args.ffx, '-c', 'ffx_repository=true', 'repository', 'default',
                'get')
            if default:
                print(f'Using ffx default repository: {default}')
                # We shouldn't ask to delete the ffx default repo.
                self.prompt_repo_cleanup = False
            return default

        # 3. A user prompt.
        def prompt_repo():
            print(
                '--repo_name was not specified and there is no default ffx repository set.'
            )
            repo_name = input('Please specify a repo name to publish to: ')
            if args.make_repo_default is None:
                args.make_repo_default = input(
                    'Would you make this repo your default ffx repo? (y/n): '
                ).lower() == 'y'
            # We shouldn't ask to delete a repo that we're going to make default.
            self.prompt_repo_cleanup = not args.make_repo_default
            return repo_name

        args.repo_name = user_specified_repo_name() or ffx_default_repo(
        ) or prompt_repo()

        # Determine the pm repo path (use the existing one from ffx, or create a new one).
        existing_repos = json.loads(
            run(
                args.ffx,
                '--machine',
                'json',
                'repository',
                'list',
            ))
        existing_repo = (
            [repo for repo in existing_repos if repo['name'] == args.repo_name]
            or [None])[0]
        existing_repo_path = existing_repo['spec'] and (
            Path(existing_repo['spec']['metadata_repo_path']).parent
            if 'metadata_repo_path' in existing_repo['spec'] else Path(
                existing_repo['spec']['path']))
        args.repo_path = existing_repo_path or Path(tempfile.mkdtemp())

    def ensure_repo(self, args):
        # Ensure repository.
        if (args.repo_path / 'repository').is_dir():
            print(f'Using existing repo: {args.repo_path}')
            return
        else:
            print(f'Creating a new repository: {args.repo_path}')
            run(args.pm, 'newrepo', '-vt', '-repo', args.repo_path)

        # Ensure ffx repository.
        print(f'Associating {args.repo_name} to {args.repo_path}')
        run(
            args.ffx,
            'repository',
            'add-from-pm',
            '--repository',
            args.repo_name,
            args.repo_path,
        )

        # Ensure ffx target repository.
        print(f'Registering {args.repo_name} to target device {args.target}')
        run(
            args.ffx,
            '--target',
            args.target,
            'target',
            'repository',
            'register',
            '-r',
            args.repo_name,
        )

        # Optionally make the ffx repository default.
        if args.make_repo_default:
            old_default = run(args.ffx, 'repository', 'default', 'get')
            print(
                f'Setting default ffx repository "{old_default}" => "{args.repo_name}"'
            )
            run(
                args.ffx,
                'repository',
                'default',
                'set',
                args.repo_name,
            )

    def publish_packages(self, args):
        # TODO(fxbug.dev/110617): Publish all packages with 1 command invocation.
        print(f'Publishing packages: {args.packages}')
        for package in args.packages:
            run(
                args.pm,
                'publish',
                '-vt',
                '-a',
                '-f',
                package,
                '-repo',
                args.repo_path,
            )
        print(f'Published {len(args.packages)} packages')

    def teardown(self, args):
        if self.prompt_repo_cleanup:
            input('Press enter to delete this repository, or ^C to quit.')

            # Delete the pm repository.
            rmtree(args.repo_path)
            print(f'Deleted {args.repo_path}')

            # Remove the ffx repository.
            run('ffx', 'repository', 'remove', args.repo_name)
            print(f'Removed the ffx repository {args.repo_name}')

    def run(self, parser: ScopedArgumentParser) -> None:
        # Parse arguments.
        args = self.parse_args(parser)

        # Check environment and gather information for publishing.
        self.enable_ffx_repository(args)
        self.ensure_target_device(args)
        self.resolve_repo(args)

        # Perform the publishing.
        self.ensure_repo(args)
        self.publish_packages(args)

        self.workflow_state['environment_variables'][
            'FUCHSIA_REPO_NAME'] = args.repo_name

        # Optionally cleanup the repo.
        self.teardown(args)


if __name__ == '__main__':
    FuchsiaTaskPublish.main()
