# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules for defining post processing script."""

load(
    ":providers.bzl",
    "FuchsiaPostProcessingScriptInfo",
)

def _fuchsia_post_processing_script_impl(ctx):
    return [
        FuchsiaPostProcessingScriptInfo(
            post_processing_script_path = ctx.attr.post_processing_script_path,
            post_processing_script_args = ctx.attr.post_processing_script_args,
            post_processing_script_inputs = ctx.attr.post_processing_script_inputs,
        ),
    ]

fuchsia_post_processing_script = rule(
    doc = """Generates post processing script target.""",
    implementation = _fuchsia_post_processing_script_impl,
    provides = [FuchsiaPostProcessingScriptInfo],
    attrs = {
        "post_processing_script_path": attr.string(
            doc = """Path to post processing script. Note: resultant zbi probably can't
            be used with tools like scrutiny, because we won't know how to unpack the zbi
            out of whatever the post-processing script has done to it.""",
        ),
        "post_processing_script_args": attr.string_list(
            doc = "Post processing script arguments",
        ),
        "post_processing_script_inputs": attr.label_keyed_string_dict(
            doc = """Dictionary for artifacts used by post processing script.
            It will be a source -> destination map""",
            allow_files = True,
        ),
    },
)
