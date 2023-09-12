# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "cipd_platform_name")

def _rustfmt(ctx):
    """Runs rustfmt on a Rust code base.

    Args:
      ctx: A ctx instance.
    """
    rust_files = [
        f
        for f in ctx.scm.affected_files()
        if f.endswith(".rs") and
           not f.startswith("third_party/") and
           # fidlgen_banjo Rust templates have an ".rs" extension but are not
           # valid Rust.
           not f.startswith("src/devices/tools/fidlgen_banjo/src/backends/templates/rust")
    ]
    if not rust_files:
        return

    base_cmd = [
        "prebuilt/third_party/rust/%s/bin/rustfmt" % cipd_platform_name(ctx),
        "--config-path",
        "rustfmt.toml",
        "--unstable-features",
        "--skip-children",
    ]

    res = ctx.os.exec(base_cmd + ["--check", "--files-with-diff"] + rust_files, ok_retcodes = [0, 1]).wait()
    unformatted = res.stdout.splitlines()
    if res.retcode and not unformatted:
        fail("rustfmt failed:\n%s" % res.stderr)
    for f in unformatted:
        filepath = f[len(ctx.scm.root) + 1:]
        output = ctx.os.exec(base_cmd + ["--emit", "stdout", filepath]).wait().stdout

        # First two lines are file name and a blank line.
        formatted = "\n".join(output.split("\n")[2:])
        ctx.emit.finding(
            # Switch to "error" if it's decided that rustfmt should be enforced
            # in presubmit.
            level = "warning",
            message = FORMATTER_MSG,
            filepath = filepath,
            replacements = [formatted],
        )

def register_rust_checks():
    shac.register_check(shac.check(_rustfmt, formatter = True))
