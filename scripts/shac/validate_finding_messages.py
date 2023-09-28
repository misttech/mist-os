# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Parses a shac Starlark file and checks that all `ctx.emit.finding()` calls
specify a `message`.

Since Starlark's syntax is a subset of Python's, we can use the Python ast
library to parse Starlark.
"""

import ast
import json
import sys


def main():
    findings = []
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
            findings.append(
                {
                    "message": "`message` argument to `ctx.emit.finding()` must be set.",
                    "line": node.lineno,
                    "end_line": node.end_lineno,
                    "col": node.col_offset + 1,
                    "end_col": node.end_col_offset + 1,
                }
            )
    print(json.dumps(findings, indent=2))


if __name__ == "__main__":
    main()
