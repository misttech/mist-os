# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "cipd_platform_name")
load("./go.star", "register_go_checks")
load("./python.star", "register_python_checks")
load("./starlark.star", "register_starlark_checks")

def _gn_format(ctx):
    """Runs gn format on .gn and .gni files.

    Args:
      ctx: A ctx instance.
    """
    gn_files = [
        f
        for f in ctx.scm.affected_files()
        if f.endswith((".gn", ".gni"))
    ]
    if not gn_files:
        return

    gn = "prebuilt/third_party/gn/%s/gn" % cipd_platform_name(ctx)

    unformatted_files = ctx.os.exec(
        [gn, "format", "--dry-run"] + gn_files,
        ok_retcodes = [0, 2],
    ).wait().stdout.splitlines()

    for f in unformatted_files:
        formatted_contents = ctx.os.exec(
            [gn, "format", "--stdin"],
            stdin = ctx.io.read_file(f),
        ).wait().stdout
        ctx.emit.finding(
            level = "error",
            message = FORMATTER_MSG,
            filepath = f,
            replacements = [formatted_contents],
        )

def register_all_checks():
    """Register all checks that should run.

    Checks must be registered in a callback function because they can only be
    registered by the root shac.star file, not at the top level of any `load`ed
    file.
    """
    shac.register_check(shac.check(_gn_format, formatter = True))
    register_go_checks()
    register_python_checks()
    register_starlark_checks()
