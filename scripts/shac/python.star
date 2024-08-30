# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "cipd_platform_name", "get_fuchsia_dir", "os_exec")

def _pyfmt(ctx):
    """Formats Python code using Black and sorts imports using isort on a Python code base.

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

    # Isort and black make conflicting code style changes. To ensure consistent formatting:
    # 1. Run isort to sort imports and apply its formatting rules.
    # 2. Run black on the formatted code to enforce its code style guidelines.
    fuchsia_dir = get_fuchsia_dir(ctx)

    isort_cmd = [
        "%s/prebuilt/third_party/python3/%s/bin/python3" % (
            fuchsia_dir,
            cipd_platform_name(ctx),
        ),
        "%s/third_party/pylibs/isort/main.py" % fuchsia_dir,
        "--stdout",
    ]
    isort_procs = [(f, os_exec(ctx, isort_cmd + [f])) for f in py_files]

    black_cmd = [
        "%s/prebuilt/third_party/black/%s/black" % (
            fuchsia_dir,
            cipd_platform_name(ctx),
        ),
        "--config",
        "%s/pyproject.toml" % fuchsia_dir,
        "-",
    ]
    black_procs = []
    for filepath, proc in isort_procs:
        isort_formatted = proc.wait().stdout
        black_procs.append(
            (filepath, os_exec(ctx, black_cmd, stdin = isort_formatted)),
        )

    for filepath, proc in black_procs:
        original = str(ctx.io.read_file(filepath))
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
