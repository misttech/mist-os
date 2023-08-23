# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaBoardConfigDirectoryInfo",
    "FuchsiaBoardConfigInfo",
)
load(":util.bzl", "extract_labels", "replace_labels_with_files")

def _fuchsia_board_configuration_impl(ctx):
    filesystems = json.decode(ctx.attr.filesystems)
    replace_labels_with_files(filesystems, ctx.attr.filesystems_labels)

    board_config = {}
    board_config["name"] = ctx.attr.board_name
    board_config["provided_features"] = ctx.attr.provided_features
    if filesystems != {}:
        board_config["filesystems"] = filesystems

    board_config_file = ctx.actions.declare_file(ctx.label.name + "_board_config.json")
    content = json.encode_indent(board_config, indent = "  ")
    ctx.actions.write(board_config_file, content)

    return [
        DefaultInfo(
            files = depset(
                direct = [board_config_file] + ctx.files.filesystems_labels,
            ),
        ),
        FuchsiaBoardConfigInfo(
            board_config = board_config_file,
        ),
    ]

_fuchsia_board_configuration = rule(
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
        "filesystems": attr.string(
            doc = "The filesystem configuration options provided by the board.",
            default = "{}",
        ),
        "filesystems_labels": attr.label_keyed_string_dict(
            doc = """Map of labels to LABEL(label) strings in the filesystems config.""",
            allow_files = True,
            default = {},
        ),
    },
)

def fuchsia_board_configuration(
        name,
        board_name,
        provided_features = [],
        filesystems = {}):
    """A board configuration that takes a dict for the filesystems config."""
    filesystem_labels = extract_labels(filesystems)
    _fuchsia_board_configuration(
        name = name,
        board_name = board_name,
        provided_features = provided_features,
        filesystems = json.encode_indent(filesystems, indent = "    "),
        filesystems_labels = filesystem_labels,
    )

def _fuchsia_prebuilt_board_configuration_impl(ctx):
    return [
        FuchsiaBoardConfigDirectoryInfo(
            config_directory = ctx.files.board_config_directory,
        ),
    ]

_fuchsia_prebuilt_board_configuration = rule(
    doc = """A prebuilt board configuration file and its main hardware support bundle.""",
    implementation = _fuchsia_prebuilt_board_configuration_impl,
    provides = [FuchsiaBoardConfigDirectoryInfo],
    attrs = {
        "board_config_directory": attr.label(
            doc = "Path to the directory containing the prebuilt board configuration.",
            mandatory = True,
        ),
    },
)

def fuchsia_prebuilt_board_configuration(
        name,
        directory):
    """A board configuration that has been prebuilt and exists in a specific folder."""
    _fuchsia_prebuilt_board_configuration(name = name, board_config_directory = directory)
