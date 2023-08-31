# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

_FORMATTER_MSG = "File not formatted. Run `fx format-code` to fix."

def _cipd_platform_name(ctx):
    """Returns CIPD's name for the current host platform.

    This is the platform name that appears in most prebuilt paths.
    """
    os = {
        "darwin": "mac",
    }.get(ctx.platform.os, ctx.platform.os)
    arch = {
        "amd64": "x64",
    }.get(ctx.platform.arch, ctx.platform.arch)
    return "%s-%s" % (os, arch)

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

    gn = "prebuilt/third_party/gn/%s/gn" % _cipd_platform_name(ctx)

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
            message = _FORMATTER_MSG,
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
