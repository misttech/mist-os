# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Shac checks that operate on C/C++ files."""

#load("//third_party/shac-project/checks-cpp/src/register.star", "register_clang_format")
load("./common.star", "FORMATTER_MSG", "cipd_platform_name", "get_fuchsia_dir")

def _clang_format_tool(ctx):
    return "%s/prebuilt/third_party/clang/%s/bin/clang-format" % (
        get_fuchsia_dir(ctx),
        cipd_platform_name(ctx),
    )

def register_cpp_checks():
    # TODO(b/338231347): Unable to currently turn this on in fuchsia.git.
    #register_clang_format(
    #    tool_ctx = _clang_format_tool,
    #    emit_message = FORMATTER_MSG,
    #    emit_level = "warning",
    #)
    pass
