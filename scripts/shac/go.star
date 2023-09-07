# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "cipd_platform_name")

def _gofmt(ctx):
    """Runs gofmt on a Go code base.

    Args:
      ctx: A ctx instance.
    """
    go_files = [f for f in ctx.scm.affected_files() if f.endswith(".go") and not f.startswith("third_party/")]
    if not go_files:
        return

    base_cmd = [
        "prebuilt/third_party/go/%s/bin/gofmt" % cipd_platform_name(ctx),
        "-s",  # simplify
    ]

    unformatted = ctx.os.exec(base_cmd + ["-l"] + go_files).wait().stdout.splitlines()
    for f in unformatted:
        new_contents = ctx.os.exec(base_cmd + [f]).wait().stdout
        ctx.emit.finding(
            level = "error",
            message = FORMATTER_MSG,
            filepath = f,
            replacements = [new_contents],
        )

def register_go_checks():
    shac.register_check(shac.check(_gofmt, formatter = True))
