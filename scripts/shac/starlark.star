# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "cipd_platform_name")

def _buildifier(ctx):
    """Checks Starlark/Bazel file formatting using buildifier."""
    starlark_files = [
        f
        for f in ctx.scm.affected_files()
        if (
               f.endswith((".star", ".bzl", ".bazel", ".bzlmod")) or
               f.split("/")[-1] in ("/BUILD", "/WORKSPACE")
           ) and
           not f.startswith("third_party/")
    ]
    if not starlark_files:
        return

    base_cmd = [
        "prebuilt/third_party/buildifier/%s/buildifier" % cipd_platform_name(ctx),
        "-lint=off",
    ]

    res = ctx.os.exec(
        base_cmd + ["-mode=check"] + starlark_files,
        ok_retcodes = (0, 4),
    ).wait()
    if res.retcode == 0:
        return

    lines = res.stderr.splitlines()
    suffix = " # reformat"

    tempfiles = {}
    for line in lines:
        if not line.endswith(suffix):
            continue
        filepath = line[:-len(suffix)]

        # Buildifier doesn't have a dry-run mode that prints the formatted file
        # to stdout, so copy each file to a temporary file and format the
        # temporary file in-place to obtain the formatted result.
        tempfiles[filepath] = ctx.io.tempfile(
            ctx.io.read_file(filepath),
            name = filepath,
        )

    ctx.os.exec(base_cmd + tempfiles.values()).wait()

    for filepath, temp in tempfiles.items():
        formatted = ctx.io.read_file(temp)
        ctx.emit.finding(
            level = "error",
            filepath = filepath,
            message = FORMATTER_MSG,
            replacements = [str(formatted)],
        )

def _validate_finding_messages(ctx):
    """Validates that `message` is set for all shac ctx.emit.finding() calls.

    shac requires `message` except for findings emitted by formatters, in which
    case it provides a default message saying to run `shac fmt`. However, `shac
    fmt` isn't exposed to fuchsia developers directly, instead they should use
    `fx format-code`, so we should never fall back to the default message.
    """
    starlark_files = [
        f
        for f in ctx.scm.affected_files()
        if f.endswith(".star")
    ]
    procs = []
    for f in starlark_files:
        procs.append(
            (f, ctx.os.exec([
                "prebuilt/third_party/python3/%s/bin/python3" % cipd_platform_name(ctx),
                "scripts/shac/validate_finding_messages.py",
                f,
            ])),
        )

    for f, proc in procs:
        res = proc.wait()
        for finding in json.decode(res.stdout):
            ctx.emit.finding(
                level = "error",
                filepath = f,
                message = finding["message"],
                line = finding["line"],
                col = finding["col"],
                end_line = finding["end_line"],
                end_col = finding["end_col"],
            )

def register_starlark_checks():
    shac.register_check(shac.check(_buildifier, formatter = True))
    shac.register_check(shac.check(_validate_finding_messages))
