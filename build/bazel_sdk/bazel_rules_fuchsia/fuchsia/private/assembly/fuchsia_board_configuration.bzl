# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules for defining assembly board configuration."""

load("@bazel_skylib//lib:paths.bzl", "paths")
load(
    "//fuchsia/private/licenses:common.bzl",
    "check_type",
)
load(
    ":providers.bzl",
    "FuchsiaBoardConfigInfo",
    "FuchsiaBoardInputBundleInfo",
    "FuchsiaPostProcessingScriptInfo",
)
load(
    ":utils.bzl",
    "LOCAL_ONLY_ACTION_KWARGS",
    "extract_labels",
    "replace_labels_with_files",
    "select_single_file",
)

def _copy_directory(ctx, src, dst, inputs, outputs, subdirectories_to_skip = None):
    cmd = """\
if [ ! -d \"$1\" ]; then
    echo \"Error: $1 is not a directory\"
    exit 1
fi

rm -rf \"$2\" && cp -fR \"$1/\" \"$2\";
"""
    if subdirectories_to_skip:
        for d in subdirectories_to_skip:
            cmd += 'rm -rf \"' + src + "/" + d + '\";\n'
    mnemonic = "CopyDirectory"
    progress_message = "Copying directory %s" % src

    ctx.actions.run_shell(
        inputs = inputs,
        outputs = outputs,
        command = cmd,
        arguments = [src, dst],
        mnemonic = mnemonic,
        progress_message = progress_message,
        use_default_shell_env = True,
        **LOCAL_ONLY_ACTION_KWARGS
    )

def _fuchsia_board_configuration_impl(ctx):
    build_id_dirs = []

    # TODO(https://fxbug.dev/349939865): Change the file name to
    # `board_configuration.json` and nest under `ctx.label.name`.
    board_config_file = ctx.actions.declare_file(ctx.label.name + "_board_config.json")
    board_files = [board_config_file]

    filesystems = json.decode(ctx.attr.filesystems)
    check_type(filesystems, "dict")
    replace_labels_with_files(filesystems, ctx.attr.filesystems_labels)
    for label, _ in ctx.attr.filesystems_labels.items():
        src = label.files.to_list()[0]
        dest = ctx.actions.declare_file(src.path)
        ctx.actions.symlink(output = dest, target_file = src)
        board_files.append(src)
        board_files.append(dest)

    board_config = {}
    board_config["name"] = ctx.attr.board_name
    board_config["provided_features"] = ctx.attr.provided_features
    if filesystems != {}:
        board_config["filesystems"] = filesystems

    kernel = json.decode(ctx.attr.kernel)
    check_type(kernel, "dict")
    if kernel != {}:
        board_config["kernel"] = kernel

    platform = json.decode(ctx.attr.platform)
    check_type(platform, "dict")
    if platform != {}:
        board_config["platform"] = platform

    hardware_info = json.decode(ctx.attr.hardware_info)
    check_type(hardware_info, "dict")
    if hardware_info != {}:
        board_config["hardware_info"] = hardware_info

    if ctx.attr.tee_trusted_app_guids:
        board_config["tee_trusted_app_guids"] = ctx.attr.tee_trusted_app_guids

    # Files from board_input_bundles have paths that are relative to root,
    # prefix "../"s to make them relative to the output board config.
    board_config_relative_to_root = "../" * board_config_file.path.count("/")
    for bib in ctx.attr.board_input_bundles:
        path = bib[FuchsiaBoardInputBundleInfo].directory
        board_config["input_bundles"] = board_config.get("input_bundles", []) + [
            board_config_relative_to_root + path,
        ]
        build_id_dirs.extend(bib[FuchsiaBoardInputBundleInfo].build_id_dirs)
    board_files.extend(ctx.files.board_input_bundles)

    if ctx.attr.devicetree:
        board_files.append(ctx.file.devicetree)
        board_config["devicetree"] = board_config_relative_to_root + ctx.file.devicetree.path

    if ctx.attr.devicetree_overlay:
        board_files.append(ctx.file.devicetree_overlay)
        board_config["devicetree_overlay"] = board_config_relative_to_root + ctx.file.devicetree_overlay.path

    args = []
    if ctx.attr.post_processing_script:
        script = ctx.attr.post_processing_script[FuchsiaPostProcessingScriptInfo]
        filesystems = board_config.get("filesystems", {})
        board_config["filesystems"] = filesystems

        zbi = filesystems.get("zbi", {})
        zbi["postprocessing_script"] = {
            "board_script_path": "scripts/" + script.post_processing_script_path,
            "args": script.post_processing_script_args,
        }
        board_config["filesystems"]["zbi"] = zbi

        paths_map = {}
        for source, dest in script.post_processing_script_inputs.items():
            # The sources come from two resource: passed in Label or python related files. If
            # the source is a file, we directly use it, otherwise it is a label, we pick
            # the first item of the files, and copy it into board configuration directory.
            if type(source) == "File":
                source_file = source
            else:
                source_file = source.files.to_list()[0]

            board_files.append(source_file)
            paths_map[source_file.path] = dest

        board_scripts_input_file = ctx.actions.declare_file(ctx.label.name + "_script_inputs.json")
        ctx.actions.write(board_scripts_input_file, json.encode(paths_map))
        board_files.append(board_scripts_input_file)

        args += [
            "--script-inputs-path",
            board_scripts_input_file.path,
        ]

    content = json.encode_indent(board_config, indent = "  ")
    ctx.actions.write(board_config_file, content)

    board_config_dir = ctx.actions.declare_directory(ctx.label.name + "_board_configuration")
    args += [
        "--config-file",
        board_config_file.path,
        "--output-dir",
        board_config_dir.path,
    ]

    ctx.actions.run(
        outputs = [board_config_dir],
        inputs = board_files,
        executable = ctx.executable._establish_board_config_dir,
        arguments = args,
        progress_message = "Build board configuration for %s" % ctx.label,
        **LOCAL_ONLY_ACTION_KWARGS
    )
    board_files.append(board_config_dir)

    return [
        DefaultInfo(
            files = depset(board_files),
        ),
        FuchsiaBoardConfigInfo(
            config = board_config_dir.path + "/board_configuration.json",
            files = board_files,
            build_id_dirs = build_id_dirs,
        ),
        OutputGroupInfo(
            build_id_dirs = depset(transitive = build_id_dirs),
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
        "hardware_info": attr.string(
            doc = "Data provided via the 'fuchsia.hwinfo.Board' protocol.",
            default = "{}",
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
        "devicetree_overlay": attr.label(
            doc = "Devicetree binary overlay (.dtbo) file",
            allow_single_file = True,
        ),
        "post_processing_script": attr.label(
            doc = "The post processing script to be included into the board configuration",
            providers = [FuchsiaPostProcessingScriptInfo],
        ),
        "platform": attr.string(
            doc = "The platform related configuration provided by the board.",
            default = "{}",
        ),
        "kernel": attr.string(
            doc = "The kernel related configuration provided by the board.",
            default = "{}",
        ),
        "tee_trusted_app_guids": attr.string_list(
            doc = "GUIDs for the TAs provided by this board's TEE driver.",
            default = [],
        ),
        "_establish_board_config_dir": attr.label(
            default = "//fuchsia/tools:establish_board_config_dir",
            executable = True,
            cfg = "exec",
        ),
    },
)

def fuchsia_board_configuration(
        name,
        filesystems = {},
        platform = {},
        kernel = {},
        hardware_info = {},
        **kwargs):
    """A board configuration that takes a dict for the filesystems config."""
    filesystem_labels = extract_labels(filesystems)

    _fuchsia_board_configuration(
        name = name,
        filesystems = json.encode_indent(filesystems, indent = "    "),
        platform = json.encode_indent(platform, indent = "    "),
        kernel = json.encode_indent(kernel, indent = "    "),
        hardware_info = json.encode_indent(hardware_info, indent = "    "),
        filesystems_labels = filesystem_labels,
        **kwargs
    )

def _fuchsia_prebuilt_board_configuration_impl(ctx):
    board_configuration = select_single_file(ctx.files.files, "board_configuration.json")
    return [
        FuchsiaBoardConfigInfo(
            files = ctx.files.files,
            config = board_configuration.path,
            build_id_dirs = [],
        ),
    ]

_fuchsia_prebuilt_board_configuration = rule(
    doc = """A prebuilt board configuration file and its main hardware support bundle.""",
    implementation = _fuchsia_prebuilt_board_configuration_impl,
    provides = [FuchsiaBoardConfigInfo],
    attrs = {
        "files": attr.label(
            doc = "A filegroup containing all of the files consisting of the prebuilt board configuration.",
            mandatory = True,
        ),
    },
)

def fuchsia_prebuilt_board_configuration(
        *,
        directory = None,
        **kwargs):
    """A board configuration that has been prebuilt and exists in a specific folder."""

    # TODO(chandarren): Migrate users to use `files` instead.
    if directory:
        kwargs["files"] = directory
    _fuchsia_prebuilt_board_configuration(**kwargs)

def _fuchsia_hybrid_board_configuration_impl(ctx):
    src_board_config = ctx.attr.board_configuration[FuchsiaBoardConfigInfo].config
    src_board_dir = paths.dirname(src_board_config)

    output_board_dir = ctx.label.name
    output_board_config = ctx.actions.declare_file(paths.join(output_board_dir, paths.basename(src_board_config)))

    board_outputs_without_bibs = []
    board_input_bundles_dir = "input_bundles"
    for f in ctx.attr.board_configuration[FuchsiaBoardConfigInfo].files:
        relative_path = paths.relativize(f.path, src_board_dir)
        if relative_path.startswith(board_input_bundles_dir):
            continue
        if not f.is_directory:
            output_path = paths.join(output_board_dir, relative_path)
            board_outputs_without_bibs.append(ctx.actions.declare_file(output_path))

    _copy_directory(
        ctx,
        src_board_dir,
        output_board_config.dirname,
        ctx.attr.board_configuration[FuchsiaBoardConfigInfo].files,
        board_outputs_without_bibs,
        subdirectories_to_skip = [board_input_bundles_dir],
    )

    all_outputs = board_outputs_without_bibs

    bib_outputs = []
    files_to_copy = []
    replacement_board_info = ctx.attr.replacement_board_input_bundles[FuchsiaBoardConfigInfo]
    for f in replacement_board_info.files:
        if f.is_directory:
            continue

        bib_board_dir = paths.dirname(replacement_board_info.config)
        relative_file_path = paths.relativize(f.path, bib_board_dir)

        # Copy in only the files which are under the boards directory of this
        # bundle.
        if not relative_file_path.startswith(board_input_bundles_dir):
            continue

        bib_outputs.append(ctx.actions.declare_file(paths.join(output_board_dir, relative_file_path)))
        files_to_copy.append(f)

    _copy_directory(
        ctx,
        paths.join(bib_board_dir, board_input_bundles_dir),
        paths.join(output_board_config.dirname, board_input_bundles_dir),
        files_to_copy,
        bib_outputs,
    )

    all_outputs += bib_outputs

    build_id_dirs = ctx.attr.board_configuration[FuchsiaBoardConfigInfo].build_id_dirs + ctx.attr.replacement_board_input_bundles[FuchsiaBoardConfigInfo].build_id_dirs

    return [
        DefaultInfo(
            files = depset(all_outputs),
        ),
        FuchsiaBoardConfigInfo(
            files = all_outputs,
            build_id_dirs = build_id_dirs,
            config = output_board_config.path,
        ),
    ]

_fuchsia_hybrid_board_configuration = rule(
    doc = "Combine in-tree board input bundles with a board from out-of-tree for hybrid assembly",
    implementation = _fuchsia_hybrid_board_configuration_impl,
    provides = [FuchsiaBoardConfigInfo],
    attrs = {
        "board_configuration": attr.label(
            doc = "Prebuilt board config",
            providers = [FuchsiaBoardConfigInfo],
            mandatory = True,
        ),
        "replacement_board_input_bundles": attr.label(
            doc = "In-tree board containing input bundles to replace those in `board_configuration`",
            providers = [FuchsiaBoardConfigInfo],
            mandatory = True,
        ),
    },
)

def fuchsia_hybrid_board_configuration(
        name,
        board_configuration,
        replacement_board_input_bundles,
        **kwargs):
    _fuchsia_hybrid_board_configuration(
        name = name,
        board_configuration = board_configuration,
        replacement_board_input_bundles = replacement_board_input_bundles,
        **kwargs
    )
