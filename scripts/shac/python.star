# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "cipd_platform_name", "get_fuchsia_dir", "os_exec")

def _pyfmt(ctx):
    """Formats Python code using autoflake, isort and black on a Python code base.

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

    # Format tools make conflicting code style changes. To ensure consistent formatting:
    # 1. Run Autoflake to remove unused imports and variables.
    # 2. Run isort to sort imports on the autoflake formatted code.
    # 3. Run black on the isort formatted code to enforce its code style guidelines.
    fuchsia_dir = get_fuchsia_dir(ctx)
    platform = cipd_platform_name(ctx)

    autoflake_cmd = [
        "%s/prebuilt/third_party/python3/%s/bin/python3" % (
            fuchsia_dir,
            platform,
        ),
        "%s/third_party/pylibs/autoflake/main.py" % fuchsia_dir,
        "--remove-unused-variables",
        "--remove-all-unused-imports",
        "--remove-duplicate-keys",
        "--ignore-init-module-imports",
        "--stdout",
    ]
    isort_cmd = [
        "%s/prebuilt/third_party/python3/%s/bin/python3" % (
            fuchsia_dir,
            platform,
        ),
        "%s/third_party/pylibs/isort/main.py" % fuchsia_dir,
        # The skip flag is necessary to override the default autoflake behavior,
        # which includes the "build/" directory in its skip section.
        "--skip",
        ".venvs",
        "--stdout",
        "--filename",
    ]
    black_cmd = [
        "%s/prebuilt/third_party/black/%s/black" % (
            fuchsia_dir,
            platform,
        ),
        "--config",
        "%s/pyproject.toml" % fuchsia_dir,
        "-",
    ]

    # Run autoflake on each file
    autoflake_procs = [(f, os_exec(ctx, autoflake_cmd + [f])) for f in py_files]

    # Run isort on the output of the autoflake.
    isort_procs = []
    for f, proc in autoflake_procs:
        formatted = proc.wait().stdout
        isort_procs.append(
            (f, os_exec(ctx, isort_cmd + [f, "-"], stdin = formatted)),
        )

    # Run black on the output of the isort.
    black_procs = []
    for f, proc in isort_procs:
        formatted = proc.wait().stdout
        black_procs.append(
            (f, os_exec(ctx, black_cmd, stdin = formatted)),
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
