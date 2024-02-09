#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import sys
from typing import Sequence, Dict, AbstractSet, Any
from pathlib import Path
from enum import Enum
import subprocess
import os

_CPP_EXTENSIONS = [".cc", ".c", ".cpp"]


class Action:
    """Represents an action that comes from aquery"""

    def __init__(self, action: Dict, target: Dict):
        self.label = target["label"]
        self.target_id = action["targetId"]
        self.action_key = action["actionKey"]
        self.arguments = action["arguments"]
        self.environment_vars = action["environmentVariables"]
        self.file = extract_file_from_args(self.arguments)

    def is_external(self) -> bool:
        return not (self.label.startswith("//") or self.label.startswith("@//"))


class CompDBFormatter:
    """A class that can convert the actions into compile_commands

    The actions that come from bazel are specific to bazel invocations and do
    not map to a command that can be passed directly to clangd. Specifically,
    the file paths are not relative to the output_root. This class will do a
    best guess on the paths to make sure they map to something that works with
    Fuchsia's out directory.
    """

    def __init__(self, build_dir: str, output_base: str, output_path: str):
        self.build_dir = build_dir
        self.output_base = output_base
        self.output_base_rel = os.path.relpath(output_base, build_dir)
        self.output_path = output_path
        self.output_path_rel = os.path.relpath(output_path, build_dir)

    def rewrite_file(self, action) -> str:
        if action.is_external():
            return os.path.join(self.output_base_rel, action.file)
        else:
            return os.path.join("../..", action.file)

    def maybe_rewrite_path(self, file_path, action) -> str:
        # Check to see if this is the file we are building. Need to take special
        # care here depending on if it is an external target or not.
        if file_path == action.file:
            return self.rewrite_file(action)

        # Bazel adds -iquote "." -iquote for files that are being compiled from
        # the internal repository. This changes those to point back to the root
        # of the fuchisa source tree.
        if file_path == ".":
            return "../../"

        # bazel-out needs to be checked first because it contains external/ paths
        if "bazel-out/" in file_path:
            return file_path.replace("bazel-out/", self.output_path_rel + "/")

        # Look for arguments to files in external/ paths. This is usually
        # the clang binary and include roots
        if "external/" in file_path:
            return file_path.replace(
                "external/",
                os.path.join(self.output_base_rel, "external") + "/",
            )

        # Just a regular argument
        return file_path

    def action_to_compile_commands(self, action: Action) -> Dict:
        return {
            "directory": self.build_dir,
            "file": self.rewrite_file(action),
            "arguments": [
                self.maybe_rewrite_path(arg, action) for arg in action.arguments
            ],
        }


def run(*command):
    try:
        return subprocess.check_output(
            command,
            text=True,
        ).strip()
    except subprocess.CalledProcessError as e:
        raise e


def list_to_dict(input: Sequence[Dict]) -> Dict:
    return {v["id"]: v for v in input}


def extract_file_from_args(args: Sequence[str]) -> str:
    """Finds the file in the action's arguments

    It would be nice to be able to get the single input file from the action but
    actions are type erased when they are returned in the query so we can't
    just grab the file that is being compiled from the arguments.
    """

    def get_ext(f):
        p = f.rfind(".")
        if p > 0:
            return f[p:]
        else:
            return ""

    files = [arg for arg in args if get_ext(arg) in _CPP_EXTENSIONS]
    assert len(files) == 1, "Should only be compiling a single file"
    return files[0]


def collect_actions(action_graph: Sequence[Dict]) -> Sequence[Action]:
    targets = list_to_dict(action_graph["targets"])
    actions = []
    for action_dict in action_graph["actions"]:
        target: Dict = targets[action_dict["targetId"]]
        action: Action = Action(action_dict, target)
        actions.append(action)
    return actions


def main(argv: Sequence[str]):
    parser = argparse.ArgumentParser(description="Refresh bazel compdb")

    parser.add_argument("--bazel", required=True, help="The bazel binary")
    parser.add_argument(
        "--build-dir", required=True, help="The build directory"
    )
    parser.add_argument(
        "--label", required=True, help="The bazel label to query"
    )
    args = parser.parse_args(argv)

    # TODO: make bazel not print output
    action_graph = json.loads(
        run(
            args.bazel,
            "aquery",
            "mnemonic('CppCompile',deps({label}))".format(label=args.label),
            "--output=jsonproto",
            "--ui_event_filters=-info",
            "--noshow_progress",
        )
    )

    actions = collect_actions(
        action_graph,
    )

    output_base = run(args.bazel, "info", "output_base")
    output_path = run(args.bazel, "info", "output_path")

    formatter = CompDBFormatter(
        args.build_dir,
        output_base,
        output_path,
    )

    new_compile_commands = []
    for action in actions:
        new_compile_commands.append(
            formatter.action_to_compile_commands(action)
        )

    compile_commands_dict = {}
    compile_commands_path = os.path.join(
        args.build_dir, "compile_commands.json"
    )
    with open(
        compile_commands_path,
        "r",
    ) as f:
        compile_commands = json.load(f)
        compile_commands.extend(new_compile_commands)
        for compile_command in compile_commands:
            compile_commands_dict[compile_command["file"]] = compile_command

    with open(
        compile_commands_path,
        "w",
    ) as f:
        json.dump(list(compile_commands_dict.values()), f, indent=2)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
