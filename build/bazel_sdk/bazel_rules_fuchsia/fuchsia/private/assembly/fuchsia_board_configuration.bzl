# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules for defining assembly board configuration."""

load("@bazel_skylib//lib:paths.bzl", "paths")
load(
    ":providers.bzl",
    "FuchsiaBoardConfigDirectoryInfo",
    "FuchsiaBoardConfigInfo",
    "FuchsiaBoardInputBundleInfo",
)
load(":util.bzl", "extract_labels", "replace_labels_with_files")

def _copy_bash(ctx, src, dst):
    cmd = """\
if [ ! -d \"$1\" ]; then
    echo \"Error: $1 is not a directory\"
    exit 1
fi

rm -rf \"$2\" && cp -fR \"$1/\" \"$2\"
"""
    mnemonic = "CopyDirectory"
    progress_message = "Copying directory %s" % src.path

    ctx.actions.run_shell(
        inputs = [src],
        outputs = [dst],
        command = cmd,
        arguments = [src.path, dst.path],
        mnemonic = mnemonic,
        progress_message = progress_message,
        use_default_shell_env = True,
        execution_requirements = {
            "no-remote": "1",
            "no-cache": "1",
        },
    )

def _fuchsia_board_configuration_impl(ctx):
    board_config_file = ctx.actions.declare_file(ctx.label.name + "_board_config.json")
    deps = [board_config_file]

    filesystems = json.decode(ctx.attr.filesystems)
    replace_labels_with_files(filesystems, ctx.attr.filesystems_labels)

    for label, _ in ctx.attr.filesystems_labels.items():
        src = label.files.to_list()[0]
        dest = ctx.actions.declare_file(src.path)
        ctx.actions.symlink(output = dest, target_file = src)
        deps.append(dest)

    board_config = {}
    board_config["name"] = ctx.attr.board_name
    board_config["provided_features"] = ctx.attr.provided_features
    if filesystems != {}:
        board_config["filesystems"] = filesystems

    if ctx.attr.board_bundles_dir:
        board_dir_name = ctx.file.board_bundles_dir.basename
        board_config["input_bundles"] = [board_dir_name + "/" + i for i in ctx.attr.input_bundles]
        board_dir = ctx.actions.declare_directory(board_dir_name)
        _copy_bash(ctx, ctx.file.board_bundles_dir, board_dir)
        deps.append(board_dir)

    # Files from board_input_bundles have paths that are relative to root,
    # prefix "../"s to make them relative to the output board config.
    board_config_relative_to_root = "../" * board_config_file.path.count("/")
    for bib in ctx.attr.board_input_bundles:
        path = bib[FuchsiaBoardInputBundleInfo].config.path
        if paths.basename(path) == "board_input_bundle.json":
            path = paths.dirname(path)
        board_config["input_bundles"] = board_config.get("input_bundles", []) + [
            board_config_relative_to_root + path,
        ]
        deps.extend(bib[FuchsiaBoardInputBundleInfo].files)

    if ctx.attr.devicetree:
        deps.append(ctx.file.devicetree)
        board_config["devicetree"] = board_config_relative_to_root + ctx.file.devicetree.path

    content = json.encode_indent(board_config, indent = "  ")
    ctx.actions.write(board_config_file, content)

    return [
        DefaultInfo(
            files = depset(
                direct = deps,
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
        "board_bundles_dir": attr.label(
            doc = "Directory containing all precompiled board input bundles.",
            allow_single_file = True,
        ),
        "input_bundles": attr.string_list(
            doc = "Directories of precompiled board input bundles to include, relative to `board_bundles_dir`.",
        ),
        "board_input_bundles": attr.label_list(
            doc = "Board Input Bundles targets to be included into the board.",
            providers = [FuchsiaBoardInputBundleInfo],
            default = [],
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
        "devicetree": attr.label(
            doc = "Devicetree binary (.dtb) file",
            allow_single_file = True,
        ),
    },
)

def fuchsia_board_configuration(
        name,
        board_name,
        board_bundles_dir = None,
        input_bundles = [],
        board_input_bundles = [],
        provided_features = [],
        filesystems = {},
        devicetree = None):
    """A board configuration that takes a dict for the filesystems config."""
    filesystem_labels = extract_labels(filesystems)

    _fuchsia_board_configuration(
        name = name,
        board_name = board_name,
        input_bundles = input_bundles,
        board_bundles_dir = board_bundles_dir,
        board_input_bundles = board_input_bundles,
        provided_features = provided_features,
        filesystems = json.encode_indent(filesystems, indent = "    "),
        filesystems_labels = filesystem_labels,
        devicetree = devicetree,
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
