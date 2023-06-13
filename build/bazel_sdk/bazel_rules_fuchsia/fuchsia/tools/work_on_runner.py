#!/usr/bin/env python3
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import re
import signal
import subprocess
import time


class FfxRunner():

    def __init__(self, ffx, cwd):
        # We need to get a absolute path to ffx since we are running from the
        # repository root
        self.ffx = os.path.realpath(ffx)
        self.cwd = cwd

    def run(self, *args):
        try:
            return subprocess.check_output(
                self._build_cmd(*args),
                text=True,
            ).strip()
        except subprocess.CalledProcessError as e:
            print(e.stdout)
            raise e

    def run_streamed(self, *args):
        try:
            proc = subprocess.Popen(
                self._build_cmd(*args), stdout=None, stderr=None)
            return proc.wait()
        except subprocess.CalledProcessError as e:
            print(e.stdout)
            raise e

    def _build_cmd(self, *args):
        return [self.ffx] + list(args)


class Context():

    def get_sdk_id(args, ffx_runner):
        if args.sdk_id_override:
            return args.sdk_id_override
        else:
            return ffx_runner.run("sdk", "version")

    def get_target(args, ffx_runner):
        # Attempt to figure out the target in the following order
        # - explicitly passed in via args
        # - ffx default target
        # - ffx config get emu.name
        # - "fuchsia-emulator"
        def first_valid(cmds, default):
            for cmd in cmds:
                try:
                    value = ffx_runner.run(*cmd)
                    if value and len(value) > 0:
                        # ffx returns the values quoted so remove the quotes
                        return value.replace('"', '')
                except:
                    pass
            return default

        if args.target:
            return args.target
        else:
            return first_valid(
                [["target", "default", "get"], ["config", "get", "emu.name"]],
                "fuchsia-emulator")

    def get_out_dir(args, cwd):
        if os.path.isabs(args.out):
            return args.out
        else:
            return os.path.join(cwd, args.out)

    def __init__(self, args):
        self.cwd = args.project_root
        self._ffx_runner = FfxRunner(args.ffx, cwd=self.cwd)
        self.out_dir = Context.get_out_dir(args, self.cwd)
        self.sdk_id = Context.get_sdk_id(args, self._ffx_runner)
        self.pb_name = args.product
        self.pb_path = os.path.join(
            self.out_dir, "product_bundles", f"{self.pb_name}.{self.sdk_id}")
        self.force_pb_download = args.force
        self.keep_build_config = args.keep_build_config
        self.target = Context.get_target(args, self._ffx_runner)

    def ffx(self):
        return self._ffx_runner

    def log(self, msg):
        # TODO: allow for verbose or not settings
        print(msg)


class StepEarlyExit():

    def __init__(self, reason):
        self.reason = reason


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

        # create the out directories if they do not exist
        os.makedirs(os.path.dirname(ctx.pb_path), exist_ok=True)

    def cleanup(self, ctx):
        pass


class SetDefaults(Step):

    def __init__(self) -> None:
        super().__init__()
        self.original_config_entries = {}
        self.config_keys = [
            "product.experimental", "ffx-repo-add", "ffx_repository"
        ]
        self.original_target = None

    def run(self, ctx):
        build_config_file = SetDefaults.find_build_config_path(ctx)
        if build_config_file is None:
            return StepEarlyExit("cannot find build config file. do you have a fuchsia_env.toml file?")

        ctx.log(f"Writing build config entry to {build_config_file}")

        try:
            with open(build_config_file, "r") as f:
                build_config = json.load(f)
        except:
            build_config = {}

        def set_nested_key(key, value):
            local_config = build_config
            keys = key.split(".")
            for k in keys[:-1]:
                local_config = local_config.setdefault(k, {})
            local_config[keys[-1]] = value

        ctx.log(f"Setting ffx configs to true {self.config_keys}.")
        for key in self.config_keys:
            set_nested_key(key, "true")

        set_nested_key("product.path", ctx.pb_path)

        try:
            self.original_target = ctx.ffx().run("target", "default", "get")
        except:
            self.original_target = None

        ctx.log(f"Setting {ctx.target} as default target")
        set_nested_key("target.default", ctx.target)

        ctx.log(
            f"Setting 'devhost.fuchsia.com' as the default package repository")
        set_nested_key("repository.default", "devhost.fuchsia.com")

        json_object = json.dumps(build_config, indent=4)
        with open(build_config_file, "w") as f:
            f.write(json_object)

    def cleanup(self, ctx):
        if ctx.keep_build_config:
            return
        try:
            os.remove(SetDefaults.find_build_config_path(ctx))
        except:
            pass


    def find_build_config_path(ctx):
        # tomllib was introduced in python 3.11 which is too new for most of our
        # repos. If it is not available fall back to a regex
        try:
            import tomllib
            loader = SetDefaults.build_config_from_toml
        except ImportError:
            loader = SetDefaults.build_config_from_regex

        cwd = os.path.normpath(ctx.cwd)
        while cwd != "/":
            config_file = os.path.join(cwd, "fuchsia_env.toml")
            if os.path.exists(config_file):
                break
            # move up a directory and reset our config_file
            cwd = os.path.dirname(cwd)
            config_file = None

        if config_file:
            return loader(ctx, config_file)
        else:
            return None

    def build_config_from_toml(ctx, file):
        import tomllib
        with open(file, "rb") as f:
            data = tomllib.load(f)
            try:
                config_path = data['fuchsia']['config']['build_config_path']
                if os.path.isabs(config_path):
                    return config_path
                else:
                    return os.path.join(os.path.dirname(file), config_path)
            except Exception as e:
                ctx.log('fuchsia_env.toml file does not contain fuchsia.config.build_config_path entry')
        return None

    def build_config_from_regex(ctx, file):
        regex = re.compile("build_config_path = \"(.*)\"")
        with open(file, 'r') as f:
          for line in f:
              m = regex.match(line)
              if m:
                  return m.group(1)
        return None


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
            try:
                ctx.ffx().run_streamed(
                    "product", "download", transfer_manifest, ctx.pb_path,
                    "--force")
            except:
                return StepEarlyExit("Failed to download product bundle.")
        else:
            ctx.log(
                f"Skipping download. Product bundle already exists at {ctx.pb_path}"
            )

        # Register the product bundle repository
        ctx.ffx().run("repository", "server", "start")
        ctx.ffx().run("repository", "add", ctx.pb_path)

    def cleanup(self, ctx):
        pass

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
                self._check_target_compatability(ctx, target_name)
            elif repository_registered and not target_up:
                ctx.log(
                    f"{target_name} lost. Waiting for target to come back online."
                )
                repository_registered = False

            time.sleep(5)

    def cleanup(self, ctx):
        pass

    def _check_target_compatability(self, ctx, target):
        ctx.log("checking for target compatibility")
        ctx.ffx().run("--target", target, "target", "wait", "-t", "60")
        result = json.loads(
            ctx.ffx().run("--target", target, "target", "show", "--json"))

        sdk_version = ""
        product = ""
        board = ""
        for entry in result:
            if entry["label"] == "build":
                for child in entry["child"]:
                    label = child["label"]
                    if label == "version":
                        sdk_version = child["value"]
                    elif label == "product":
                        product = child["value"]
                    elif label == "board":
                        board = child["value"]

        product_dot_board = f"{product}.{board}"

        # Ignore names that end in '-dfv2' since they are not actually different
        # boards and will always report an error.
        pb_name = ctx.pb_name.removesuffix("-dfv2")
        if ctx.sdk_id != sdk_version or pb_name != product_dot_board:
            ctx.log(
                f"WARNING: the target {target} might not be compatible with your SDK."
            )
            ctx.log(
                "If you experience unexpected errors, try updating your target or"
            )
            ctx.log("restarting this program with the correct target.")
            ctx.log(
                f"Expected SDK id: {ctx.sdk_id}, Target SDK id: {sdk_version}")
            ctx.log(
                f"Expected product: {ctx.pb_name}, Target product: {product_dot_board}"
            )
        else:
            ctx.log("Target is running expected SDK version and product.")


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
            result = step.run(self.ctx)
            if type(result) is StepEarlyExit:
                self.ctx.log("Exiting early:")
                self.ctx.log(result.reason)
                break
        self.shutdown()

    def shutdown(self):
        self.ctx.log("Shutting down")
        for step in self.progression:
            step.cleanup(self.ctx)


def main():
    parser = argparse.ArgumentParser()

    parser.add_argument(
        '--ffx',
        help='A path to the ffx tool.',
        required=True,
    )

    parser.add_argument(
        '--target', help='The name of the target which you are working on.')

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

    parser.add_argument(
        '--keep-build-config',
        help='do not clean up the build configuration when exiting',
        required=False,
        default=False,
        action='store_true')

    parser.add_argument('product', help="The name of the product/board combo")

    args = parser.parse_args()
    runner = Runner(Context(args))

    def handle_signal(signum, frame):
        runner.shutdown()
        exit(1)

    signal.signal(signal.SIGINT, handle_signal)
    runner.run()


if __name__ == '__main__':
    main()
