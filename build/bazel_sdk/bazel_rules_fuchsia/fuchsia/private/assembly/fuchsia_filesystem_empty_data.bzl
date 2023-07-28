# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":providers.bzl", "FuchsiaFsEmptyDataInfo")

def _fuchsia_filesystem_empty_data_impl(ctx):
    return [
        FuchsiaFsEmptyDataInfo(
            empty_data_name = ctx.attr.empty_data_name,
        ),
    ]

fuchsia_filesystem_empty_data = rule(
    doc = """Generates an empty data filesystem.""",
    implementation = _fuchsia_filesystem_empty_data_impl,
    provides = [FuchsiaFsEmptyDataInfo],
    attrs = {
        "empty_data_name": attr.string(
            doc = "Name of filesystem",
            default = "empty-data",
        ),
    },
)
