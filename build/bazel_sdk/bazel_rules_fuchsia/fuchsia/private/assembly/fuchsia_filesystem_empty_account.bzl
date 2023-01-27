# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaFsEmptyAccountInfo")

def _fuchsia_filesystem_empty_account_impl(ctx):
    return [
        FuchsiaFsEmptyAccountInfo(
            empty_account_name = ctx.attr.empty_account_name,
        ),
    ]

fuchsia_filesystem_empty_account = rule(
    doc = """Generates an empty account filesystem.""",
    implementation = _fuchsia_filesystem_empty_account_impl,
    provides = [FuchsiaFsEmptyAccountInfo],
    attrs = {
        "empty_account_name": attr.string(
            doc = "Name of filesystem",
            default = "account",
        ),
    },
)
