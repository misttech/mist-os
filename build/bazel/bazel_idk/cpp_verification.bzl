# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules related to verification of C++ atoms."""

def _verify_no_pragma_once_impl(ctx):
    stamp_file = ctx.actions.declare_file(ctx.label.name + ".stamp")
    args = ctx.actions.args()
    args.add("--stamp", stamp_file.path)
    args.add_all("--headers", ctx.files.files)
    ctx.actions.run(
        executable = ctx.executable._script,
        arguments = [args],
        inputs = ctx.files.files,
        outputs = [stamp_file],
        tools = [ctx.executable._script],
        mnemonic = "VerifyNoPragmaOnceInHeaders",
    )
    return [DefaultInfo(files = depset([stamp_file]))]

verify_no_pragma_once = rule(
    doc = "Verifies that a group of (header) files does not contain `#pragma once` directives.",
    implementation = _verify_no_pragma_once_impl,
    attrs = {
        "files": attr.label_list(
            doc = "The list of (header) files to check.",
            allow_files = True,
            mandatory = True,
        ),
        "_script": attr.label(
            doc = "The script to run.",
            default = "//build/cpp:verify_pragma_once",
            executable = True,
            cfg = "exec",
        ),
    },
)
