# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

def _py_shebangs(ctx):
    """Validates that all Python script shebangs specify the vendored Python interpeter.

    Scripts can opt out of this by adding a comment with
    "allow-non-vendored-python" in a line after the shebang.
    """
    ignore_paths = (
        "build/bazel/",
        "build/bazel_sdk/",
        "infra/",
        "integration/",
        "vendor/",
        "third_party/",
    )
    for path in ctx.scm.affected_files():
        if not path.endswith(".py"):
            continue
        if path.startswith(ignore_paths):
            continue
        lines = str(ctx.io.read_file(path, 4096)).splitlines()
        if not lines:
            continue
        first_line = lines[0]
        want_shebang = "#!/usr/bin/env fuchsia-vendored-python"
        if first_line.startswith("#!") and first_line != want_shebang:
            if len(lines) > 1 and lines[1].startswith("# allow-non-vendored-python"):
                continue
            ctx.emit.finding(
                level = "warning",
                message = "Use fuchsia-vendored-python in shebangs for Python scripts.",
                filepath = path,
                line = 1,
                replacements = [want_shebang + "\n"],
            )

def register_python_checks():
    shac.register_check(_py_shebangs)
