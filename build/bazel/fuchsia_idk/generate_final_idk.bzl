# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A rule to generate a final IDK directory.

The rule produces a single output directory that contains an IDK export
directory that follows the official IDK layout, meaning that its
metadata files only contain relative files paths, and no Bazel label
targets.

This rule should only be invoked from the top-level BUILD.bazel file
of a fuchsia_idk_repository() external repository, and will define
a target that will enforce that all inputs files are generated.
"""

def _generate_final_idk_impl(ctx):
    output_dir = ctx.actions.declare_directory(ctx.label.name)

    # Generate a simple shell script to perform the copy.
    copy_script = ctx.actions.declare_file(ctx.label.name + ".copy.sh")

    copy_entries = {}
    inputs_transitive = []
    for target, dst_path in ctx.attr.files_to_copy.items():
        inputs_transitive.append(target[DefaultInfo].files)
        copy_entries[dst_path] = target[DefaultInfo].files.to_list()[0]

    meta_entries = {}
    for target, dst_path in ctx.attr.meta_files_to_copy.items():
        inputs_transitive.append(target[DefaultInfo].files)
        meta_entries[dst_path] = target[DefaultInfo].files.to_list()[0]

    # A sed pattern used to replace labels into relative paths.
    # e.g. @<repo_name>//<package>:<name> --> <package>/<name>
    sed_pattern = "s|@{repo_name}//\\([^:]*\\):\\(.*\\)|\\1/\\2|g".format(repo_name = ctx.label.repo_name)

    content = """#!/bin/bash
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# AUTO-GENERATED - DO NOT EDIT
set -e

# Copy an IDK metadata file, transforming Bazel labels
# into relative paths, e.g. @fuchsia_idk//package:name -> package/name
# $1: source path
# $2: destination path
function copy_metadata {
  # Replace @repo//<package>:<name> instances in the file
  # with "<package>/<name>" instead.
  sed -e "@SED_PATTERN@" "$(realpath "$1")" > "$2"
}

# Copy (or symlink) a non-metadata IDK file
# $1: source path
# $2: destination path
function copy_file {
  ln -sf "$(realpath "$1")" "$2"
}

""".replace("@SED_PATTERN@", sed_pattern)

    # Create all output sub-directories first.
    parent_dirs = {}
    for dst_path in copy_entries.keys() + meta_entries.keys():
        dirname, sep, basename = dst_path.rpartition("/")
        if sep == "/":
            parent_dirs[dirname] = True

    for dst_dir in sorted(parent_dirs.keys()):
        content += "mkdir -p {out_dir}/{dst_dir}\n".format(
            out_dir = output_dir.path,
            dst_dir = dst_dir,
        )

    # Copy IDK manifest
    content += "copy_file {src_path} {out_dir}/meta/manifest.json\n".format(
        src_path = ctx.file.manifest.path,
        out_dir = output_dir.path,
    )

    # Copy all metadata files.
    for dst_path, src_file in sorted(meta_entries.items()):
        content += "copy_metadata {src_path} {out_dir}/{dst_path}\n".format(
            src_path = src_file.path,
            dst_path = dst_path,
            out_dir = output_dir.path,
        )

    # Copy all other files.
    for dst_path, src_file in sorted(copy_entries.items()):
        content += "copy_file {src_path} {out_dir}/{dst_path}\n".format(
            src_path = src_file.path,
            dst_path = dst_path,
            out_dir = output_dir.path,
        )

    ctx.actions.write(copy_script, content, is_executable = True)

    # Then invoke it
    ctx.actions.run(
        outputs = [output_dir],
        inputs = depset([], transitive = inputs_transitive),
        executable = copy_script,
        mnemonic = "FinalizeIDK",
        # No need for remoting or sanboxing here. The IDK is over 2 GiB
        # and this operation creates a tree of symlinks inside a TreeArtifact
        # that does not benefit from local sandboxing.
        execution_requirements = {
            "local": "1",
        },
    )

    return [
        DefaultInfo(
            files = depset([output_dir]),
        ),
    ]

generate_final_idk = rule(
    implementation = _generate_final_idk_impl,
    doc = "Generate an IDK directory that can be used OOT, i.e. whose metadata files do not include any Bazel target labels",
    attrs = {
        "manifest": attr.label(
            doc = "Location of the input IDK manifest file.",
            mandatory = True,
            allow_single_file = True,
        ),
        "files_to_copy": attr.label_keyed_string_dict(
            doc = "A { target_label -> dest_path } mapping, where target_label is a Bazel label to eithe a file or a target that generates a single file.",
            allow_files = True,
        ),
        "meta_files_to_copy": attr.label_keyed_string_dict(
            doc = "A { target_label -> dest_path } mapping, where target_label is a Bazel label to either a file or a target for an IDK metadata file.",
            allow_files = True,
        ),
    },
)
