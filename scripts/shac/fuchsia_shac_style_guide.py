# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Checks a shac Starlark file for fuchsia-specific issues.

Since Starlark's syntax is a subset of Python's, we can use the Python ast
library to parse Starlark.
"""

import ast
import collections
import json
import sys


def main():
    findings = collections.defaultdict(list)
    path = sys.argv[1]
    with open(path) as f:
        contents = f.read()

    tree = ast.parse(contents)
    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Call)
            and ast.unparse(node).startswith("ctx.emit.finding(")
            and not any(kw.arg == "message" for kw in node.keywords)
        ):
            # shac requires `message` except for findings emitted by formatters,
            # in which case it provides a default message saying to run `shac
            # fmt`. However, `shac fmt` isn't exposed to fuchsia developers
            # directly, instead they should use `fx format-code`, so we should
            # never fall back to the default message.
            findings[node].append(
                "`message` argument to `ctx.emit.finding()` must be set."
            )

        if isinstance(node, ast.Call) and ast.unparse(node).startswith(
            "print("
        ):
            findings[node].append(
                "Do not commit shac check code that calls `print()`, "
                "it's only to be used for debugging."
            )

    print(
        json.dumps(
            [
                {
                    "message": msg,
                    "line": node.lineno,
                    "col": node.col_offset + 1,
                    "end_line": node.end_lineno,
                    "end_col": node.end_col_offset + 1,
                }
                for node, msgs in findings.items()
                for msg in msgs
            ],
            indent=2,
        )
    )


def location_finding_kwargs(node: ast.AST):
    res = {
        "line": node.lineno,
        "col": node.col_offset + 1,
    }
    if node.end_lineno:
        res["end_line"] = node.end_lineno
    if node.end_col_offset:
        res["end_col"] = node.end_col_offset + 1
    return res


if __name__ == "__main__":
    main()
