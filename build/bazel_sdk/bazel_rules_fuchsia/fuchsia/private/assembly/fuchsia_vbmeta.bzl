# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load(":providers.bzl", "FuchsiaVbmetaInfo")

def _fuchsia_vbmeta_impl(ctx):
    return [
        FuchsiaVbmetaInfo(
            vbmeta_name = ctx.attr.vbmeta_name,
            key = ctx.file.key,
            key_metadata = ctx.file.key_metadata,
        ),
    ]

fuchsia_vbmeta = rule(
    doc = """Generates a fuchsia vbmeta image.""",
    implementation = _fuchsia_vbmeta_impl,
    provides = [FuchsiaVbmetaInfo],
    attrs = {
        "vbmeta_name": attr.string(
            doc = "Name of vbmeta image appeared in image configuration",
            default = "fuchsia",
        ),
        "key": attr.label(
            doc = "the key for signing VBMeta.",
            allow_single_file = True,
            default = None,
        ),
        "key_metadata": attr.label(
            doc = "key metadata to add to the VBMeta.",
            allow_single_file = True,
            default = None,
        ),
    },
)
