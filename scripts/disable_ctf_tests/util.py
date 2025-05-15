# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

""" Various useful bits and pieces. """

import enum
import importlib.util
import json
import os
import sys
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

""" The state-save file is at a fixed location in the source tree. """
_JSON_FILE = "sdk/ctf/disabled_tests.json"


# import json_get from its location in the source tree
# This avoids requiring the script user to mess with Python paths.
# Assuming this succeeds, util.JsonGet will be the json_get.JsonGet class.
source_directory = os.path.dirname(os.path.abspath(__file__))
assert source_directory.endswith(
    "scripts/disable_ctf_tests"
), f"Unexpected script location {source_directory}"
json_get_path = os.path.join(
    source_directory, "..", "lib", "json_get", "json_get.py"
)
spec = importlib.util.spec_from_file_location("json_get", json_get_path)
if spec and spec.loader:
    json_get = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(
        json_get
    )  # Execute the module to make its contents available
else:
    print(f"Could not load json_get from {json_get_path}")
    sys.exit(1)

if TYPE_CHECKING:

    class JsonGet:
        def __init__(self, text: str, value: Any = None) -> None:
            pass

        def match(
            self, pattern: Any, callback: Any = None, no_match: Any = None
        ) -> Any:
            return None

else:
    JsonGet = json_get.JsonGet


class Status(enum.Enum):
    """Generic status values that can be returned from poll() functions."""

    WORKING = 1  # Haven't yet succeeded or failed
    FAILING = 2  # Detected failure, but haven't finalized
    SUCCESS = 3  # Finalized; worked as expected
    FAILURE = 4  # Finalized unsuccessfully


@dataclass
class Outputs:
    """Transfer human-readable information from business logic to UI.

    - updates are current status - in a GUI, the user probably just needs to
        see the latest.
    - prints are more durable - the user may want to see all of them.
    - errors mean something has gone wrong; the program is probably about to
        quit, and the user needs to know why.
    """

    updates: list[str] = field(default_factory=list)
    prints: list[str] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)

    def update(self, s: str) -> None:
        self.updates.append(s)

    def print(self, p: str) -> None:
        self.prints.append(p)

    def error(self, e: str) -> None:
        self.errors.append(e)


def _vars_or_obj(o: Any) -> Any:
    """Convert object/data into a json.dump'able thing."""
    try:
        return vars(o)
    except:
        return o


def save_program_state(state: Any) -> None:
    """Write state into a JSON-formatted file."""
    with open(_JSON_FILE, "w") as f:
        json.dump(state, f, default=_vars_or_obj, indent=2)


def load_program_state() -> Any:
    """Load the state-save file into JSON data."""
    with open(_JSON_FILE) as f:
        return json.load(f)
