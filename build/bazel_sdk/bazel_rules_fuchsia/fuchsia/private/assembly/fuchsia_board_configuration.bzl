# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaBoardConfigInfo",
)

def _fuchsia_board_configuration_impl(ctx):
    board_config = {}
    board_config["name"] = ctx.attr.board_name
    board_config["provided_features"] = ctx.attr.provided_features

    board_config_file = ctx.actions.declare_file(ctx.label.name + "_board_config.json")
    content = json.encode_indent(board_config, indent = "  ")
    ctx.actions.write(board_config_file, content)

    return [
        DefaultInfo(
            files = depset(
                direct = [board_config_file],
            ),
        ),
        FuchsiaBoardConfigInfo(
            board_config = board_config_file,
        ),
    ]

fuchsia_board_configuration = rule(
    doc = """Generates a board configuration file.""",
    implementation = _fuchsia_board_configuration_impl,
    attrs = {
        "board_name": attr.string(
            doc = "Name of this board.",
            mandatory = True,
        ),
        "provided_features": attr.string_list(
            doc = "The features that this board provides to the product.",
        ),
    },
)
