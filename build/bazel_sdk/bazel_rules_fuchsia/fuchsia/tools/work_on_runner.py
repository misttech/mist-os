#!/usr/bin/env python3
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import subprocess
import os
import json
import time
import signal

from pathlib import Path


class FfxRunner():

    def __init__(self, ffx, cwd):
        # We need to get a absolute path to ffx since we are running from the
        # repository root
        self.ffx = os.path.realpath(ffx)
        print(self.ffx)
        self.cwd = cwd

    def run(self, *args, verbose_exception=True):
        try:
            cmd = [self.ffx] + list(args)
            return subprocess.check_output(
                cmd,
                text=True,
            ).strip()
        except subprocess.CalledProcessError as e:
            if verbose_exception:
                print(e.stdout)
            raise e


class Context():

    def get_sdk_id(args, ffx_runner):
        if args.sdk_id_override:
            return args.sdk_id_override
        else:
            return ffx_runner.run("sdk", "version")

    def get_out_dir(args, cwd):
        if os.path.isabs(args.out):
            return args.out
        else:
            return os.path.join(cwd, args.out)

    def __init__(self, args):
        self.cwd = args.project_root
        self._ffx_runner = FfxRunner(args.ffx, cwd=self.cwd)
        self.target = args.target
        self.out_dir = Context.get_out_dir(args, self.cwd)
        self.sdk_id = Context.get_sdk_id(args, self._ffx_runner)
        self.pb_name = args.product
        self.pb_path = os.path.join(
            self.out_dir, "product_bundles", f"{self.pb_name}.{self.sdk_id}")
        self.force_pb_download = args.force

    def ffx(self):
        return self._ffx_runner

    def log(self, msg):
        # TODO: allow for verbose or not settings
        print(msg)


class Step():

    def run(self, ctx):
        pass

    def cleanup(self, ctx):
        pass


class Startup(Step):

    def run(self, ctx):
        ctx.log(f"Starting up: ")
        ctx.log(f"- Taret SDK ID: {ctx.sdk_id}")
        ctx.log(f"- Product: {ctx.pb_name}")
        ctx.log(f"- Target: {ctx.target}")

    def cleanup(self, ctx):
        pass


class SetDefaults(Step):

    def __init__(self) -> None:
        super().__init__()
        self.original_config_entries = {}
        self.config_keys = [
            "product.experimental", "ffx-repo-add", "ffx_repository"
        ]

    def run(self, ctx):
        ctx.log(f"Setting ffx configs to true {self.config_keys}.")
        for key in self.config_keys:
            self.original_config_entries[key] = ctx.ffx().run(
                "config", "get", key)
            ctx.ffx().run("config", "set", key, "true")

        ctx.log(f"Setting {ctx.target} as default target")
        ctx.ffx().run("target", "default", "set", ctx.target)

        ctx.log(
            f"Setting 'devhost.fuchsia.com' as the default package repository")
        ctx.ffx().run("repository", "default", "set", "devhost.fuchsia.com")

    def cleanup(self, ctx):
        ctx.log("Setting ffx config values back to original values")
        for key, value in self.original_config_entries.items():
            ctx.ffx().run("config", "set", key, value)
        ctx.log("Unsetting default target")
        ctx.ffx().run("target", "default", "unset")


class FetchProductBundle(Step):

    def run(self, ctx):
        if self._download_needed(ctx):
            ctx.log(f"Downloading product bundle to {ctx.pb_path}")
            transfer_manifest = ctx.ffx().run(
                "product",
                "lookup",
                ctx.pb_name,
                ctx.sdk_id,
                "--base-url",
                f"gs://fuchsia/development/{ctx.sdk_id}",
            )
            ctx.ffx().run(
                "product", "download", transfer_manifest, ctx.pb_path,
                "--force")
        else:
            ctx.log(
                f"Skipping download. Product bundle already exists at {ctx.pb_path}"
            )

        # Write the path to the product bundle in the out dir
        with open(self.pb_config_path(ctx), 'w') as f:
            f.write(ctx.pb_path)

        # Register the product bundle repository
        ctx.ffx().run("repository", "server", "start")
        ctx.ffx().run("repository", "add", ctx.pb_path)

    def cleanup(self, ctx):
        os.remove(self.pb_config_path(ctx))

    def _download_needed(self, ctx):
        if os.path.exists(ctx.pb_path):
            return ctx.force_pb_download
        return True

    def pb_config_path(self, ctx):
        return os.path.join(ctx.out_dir, '.pb_path')


class WatchForTarget(Step):

    def run(self, ctx):
        ctx.log("Watching for target to come up")
        repository_registered = False
        target_name = ctx.target
        while True:
            try:
                found_targets = json.loads(
                    ctx.ffx().run("--machine", "JSON", "target", "list"))
            except subprocess.CalledProcessError:
                found_targets = []
            target_up = False
            for target in found_targets:
                if target['nodename'] == target_name and target[
                        'target_state'] == 'Product':
                    target_up = True
                    break

            if target_up and not repository_registered:
                print(f"{target_name} found. Registering package repository.")
                ctx.ffx().run(
                    "target", "repository", "register", "-r",
                    "devhost.fuchsia.com", "--alias", "fuchsia.com", "--alias",
                    "chromium.org")
                repository_registered = True
                ctx.log(
                    f"{target_name} found. Registered 'devhost.fuchsia.com'")
            elif repository_registered and not target_up:
                ctx.log(
                    f"{target_name} lost. Waiting for target to come back online."
                )
                repository_registered = False

            time.sleep(5)

    def cleanup(self, ctx):
        pass


class Runner:

    def __init__(self, ctx):
        self.ctx = ctx
        self.progression = [
            Startup(),
            SetDefaults(),
            FetchProductBundle(),
            WatchForTarget()
        ]

    def run(self):
        for step in self.progression:
            step.run(self.ctx)

    def shutdown(self, signum, frame):
        self.ctx.log("Shutting down")
        for step in self.progression:
            step.cleanup(self.ctx)
        exit(0)


def main():
    parser = argparse.ArgumentParser()

    parser.add_argument(
        '--ffx',
        help='A path to the ffx tool.',
        required=True,
    )

    parser.add_argument(
        '--target',
        help='The name of the target which you are working on.',
        default='fuchsia-emulator')

    parser.add_argument(
        '--out',
        help='The out directory to put the product bundle',
        default='out',
    )

    parser.add_argument(
        '--project-root',
        help='The path to the project root.',
        default=os.environ.get("BUILD_WORKSPACE_DIRECTORY"))

    parser.add_argument(
        '--sdk-id-override',
        help=
        'Allows for overriding the sdk_id used when fetching product bundles',
        required=False,
    )

    parser.add_argument(
        '--force',
        help='force downloading the product bundle',
        required=False,
        default=False,
        action='store_true')

    parser.add_argument('product', help="The name of the product/board combo")

    args = parser.parse_args()
    runner = Runner(Context(args))

    signal.signal(signal.SIGINT, runner.shutdown)
    runner.run()


if __name__ == '__main__':
    main()
