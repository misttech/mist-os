# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import sys
import typing
import os
import json

# kinds of FIDL declaration
KINDS = [
    "bits",
    "const",
    "enum",
    "experimental_resource",
    "protocol",
    "service",
    "struct",
    "table",
    "alias",
    "new_type",
    "union",
    "overlay",
]


def location_object_hook(d):
    if "location" in d:
        del d["location"]
    return d


class ObjectHook:
    keep_location: bool
    keep_documentation: bool

    def __init__(self, keep_location: bool, keep_documentation: bool):
        self.keep_location = keep_location
        self.keep_documentation = keep_documentation

    def __call__(self, d: dict) -> dict:
        if not self.keep_location and "location" in d:
            del d["location"]
        if not self.keep_documentation and "maybe_attributes" in d:
            for i in range(len(d["maybe_attributes"])):
                if d["maybe_attributes"][i]["name"] == "doc":
                    del d["maybe_attributes"][i]
                    if len(d["maybe_attributes"]) == 0:
                        del d["maybe_attributes"]
                    break
        return d


def find_declaration(name, declarations):
    for decl in declarations:
        if decl["name"] == name:
            return decl
    raise Exception(f"{name} not found")


def merge_irs(
    inputs: typing.List[typing.TextIO],
    output: typing.TextIO,
    keep_location: bool = False,
    keep_documentation: bool = False,
):
    # should we keep location and documentation?
    json_object_hook = ObjectHook(keep_location, keep_documentation)
    if keep_location and keep_documentation:
        json_object_hook = None

    # parse the inputs
    experiments = None
    available = None
    dependencies = set()
    declarations = {}
    for f in inputs:
        ir = json.load(f, object_hook=json_object_hook)
        # make sure the experiments match
        if experiments != ir.get("experiments"):
            if experiments is None:
                experiments = ir["experiments"]
            else:
                raise Exception("Experiments mismatch")
        # make sure available versions match
        if available != ir.get("available"):
            if available is None:
                available = ir["available"]
            else:
                raise Exception("Available mismatch")

        # track all of the external dependencies declared in this library
        for lib in ir["library_dependencies"]:
            dependencies.update(n for n in lib["declarations"].keys())

        # track the declarations in this library
        for name, kind in ir["declarations"].items():
            assert kind in KINDS
            decl = (kind, find_declaration(name, ir[f"{kind}_declarations"]))
            if name in declarations:
                if decl != declarations[name]:
                    raise Exception(f"conflicting definitions for {name}")
            else:
                declarations[name] = decl

    # make sure the dependencies are satisfied
    declaration_names = frozenset(declarations.keys())
    missing_dependencies = dependencies - declaration_names
    if missing_dependencies:
        raise Exception(
            "Missing declarations: " + (", ".join(sorted(missing_dependencies)))
        )

    # build merged IR
    merged: dict[str, typing.Any] = {
        f"{kind}_declarations": [] for kind in KINDS
    }
    merged["declarations"] = {}
    for name, (kind, info) in declarations.items():
        merged[f"{kind}_declarations"].append(info)
        merged["declarations"][name] = kind
    # sort declarations within kind arrays
    for kind in KINDS:
        merged[f"{kind}_declarations"].sort(key=lambda d: d["name"])

    # put back available and experiments if they were seen
    if available is not None:
        merged["available"] = available
    if experiments is not None:
        merged["experiments"] = experiments

    json.dump(merged, output, indent=2)
