# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "cipd_platform_name", "get_fuchsia_dir", "os_exec")

def _pyfmt(ctx):
    """Runs python formatter black on a Python code base.

    Args:
      ctx: A ctx instance.
    """
    py_files = [
        f
        for f in ctx.scm.affected_files()
        if f.endswith(".py") and "third_party" not in f.split("/")
    ]
    if not py_files:
        return

    procs = []
    base_cmd = [
        "%s/prebuilt/third_party/black/%s/black" % (
            get_fuchsia_dir(ctx),
            cipd_platform_name(ctx),
        ),
        "--config",
        "%s/pyproject.toml" % get_fuchsia_dir(ctx),
    ]
    for filepath in py_files:
        original = str(ctx.io.read_file(filepath))
        procs.append(
            (filepath, original, os_exec(ctx, base_cmd + ["-"], stdin = original)),
        )
    for filepath, original, proc in procs:
        formatted = proc.wait().stdout
        if formatted != original:
            ctx.emit.finding(
                level = "error",
                message = FORMATTER_MSG,
                filepath = filepath,
                replacements = [formatted],
            )

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
    shac.register_check(shac.check(_pyfmt, formatter = True))
    shac.register_check(_py_shebangs)
