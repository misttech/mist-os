# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
A set of repository rules used by the Bazel workspace for the Fuchsia
platform build.
"""

def _ninja_target_from_gn_label(gn_label):
    """Convert a GN label into an equivalent Ninja target name"""

    #
    # E.g.:
    #  //build/bazel:something(//build/toolchain/fuchsia:x64)
    #       --> build/bazel:something
    #
    #  //build/bazel/something:something(//....)
    #       --> build/bazel:something
    #
    # This assumes that all labels are in the default toolchain (since
    # otherwise the corresponding Ninja label is far too complex to compute).
    #
    ninja_target = gn_label.split("(")[0].removeprefix("//")
    dir_name, _, target_name = ninja_target.partition(":")
    if dir_name.endswith("/" + target_name):
        ninja_target = dir_name.removesuffix(target_name).removesuffix("/") + ":" + target_name
    return ninja_target

def _gn_label_decompose(gn_label):
    """Decompose a GN label into a directory name and a target name."""
    gn_label = gn_label.split("(")[0]  # Remove toolchain suffix if present.
    gn_dir, colon, gn_name = gn_label.partition(":")
    if colon != ":":
        gn_dir = gn_label
        sep = gn_label.rfind("/")
        if sep < 2:
            fail("Invalid GN label does not start with //: " + gn_label)
        gn_name = gn_label[sep + 1:]
    return gn_dir, gn_name

################################################################################
################################################################################
#####
#####    bazel_inputs_repository()
#####

def _bazel_inputs_repository_impl(repo_ctx):
    build_bazel_content = '''# Auto-generated - do not edit

load("@rules_license//rules:license.bzl", "license")

package(
    default_visibility = ["//visibility:public"],
    default_applicable_licenses = [ ":license" ],
)

exports_files(
    glob(
      ["**"],
      exclude=["ninja_output"],
      exclude_directories=0,
    )
)

license(
    name = "license",
    package_name = "Legacy Ninja Build Outputs",
    license_text = "legacy_ninja_build_outputs_licenses.spdx.json"
)

'''

    # The Ninja output directory is passed by the launcher script at
    # $BAZEL_TOPDIR/bazel as an environment variable.
    #
    # This is the root directory for all source entries in the manifest.
    # Create a //:ninja_output symlink in the repository to point to it.
    ninja_output_dir = repo_ctx.os.environ["BAZEL_FUCHSIA_NINJA_OUTPUT_DIR"]
    source_prefix = ninja_output_dir + "/"

    ninja_targets = []

    # //build/bazel/bazel_inputs.gni for the schema definition.
    for entry in json.decode(repo_ctx.read(repo_ctx.attr.inputs_manifest)):
        gn_label = entry["gn_label"]
        gn_dir, gn_name = _gn_label_decompose(gn_label)
        gn_name = entry.get("gn_targets_name", gn_name)

        content = '''# From GN target: {label}
# Migration target: @gn_targets{gn_dir}:{gn_name}
filegroup(
    name = "{name}",
'''.format(label = gn_label, name = entry["name"], gn_dir = gn_dir, gn_name = gn_name)
        if "sources" in entry:
            # A regular filegroup that list sources explicitly.
            content += "    srcs = [\n"
            for src, dst in zip(entry["sources"], entry["destinations"]):
                content += '       "{dst}",\n'.format(dst = dst)
                src_file = source_prefix + src
                repo_ctx.symlink(src_file, dst)

            content += "    ],\n"
        elif "source_dir" in entry:
            # A directory filegroup which uses glob() to group input files.
            src_dir = source_prefix + entry["source_dir"]
            dst_dir = entry["dest_dir"]
            content += '    srcs = glob(["{dst_dir}**"])\n'.format(dst_dir = dst_dir)
            repo_ctx.symlink(src_dir, dst_dir)
        else:
            fail("Invalid inputs manifest entry: %s" % entry)

        content += ")\n\n"
        build_bazel_content += content

        # Convert GN label into the corresponding Ninja target.
        ninja_targets.append(_ninja_target_from_gn_label(gn_label))

    repo_ctx.file("BUILD.bazel", build_bazel_content)
    repo_ctx.file("WORKSPACE.bazel", "")
    repo_ctx.file("MODULE.bazel", 'module(name = "{name}", version = "1"),\n'.format(name = repo_ctx.attr.name))

bazel_inputs_repository = repository_rule(
    implementation = _bazel_inputs_repository_impl,
    attrs = {
        "inputs_manifest": attr.label(
            allow_files = True,
            mandatory = True,
            doc = "Label to the inputs manifest file describing the repository's content",
        ),
    },
    doc = "A repository rule used to populate a workspace with filegroup() entries " +
          "exposing Ninja build outputs as Bazel inputs. Its content is described by " +
          "a Ninja-generated input manifest, a JSON array of objects describing each " +
          "filegroup().",
)

################################################################################
################################################################################
#####
#####    gn_targets_repository()
#####

def _gn_targets_repository_impl(repo_ctx):
    # The files generated by //build/bazel/scripts/bazel_action.py.
    # Note that these attributes are strings, not labels, because the files
    # will _not_ necessarily exist, due to the following contraints:
    #
    # - These files are modified by several distinct Ninja actions
    #   (each one corresponding to a single GN bazel_action() target), as
    #   such, they cannot be listed as outputs of any Ninja target, as this
    #   would ruin Ninja no-op checks.
    #
    # - These files may not exist in certain cases, for example, starting
    #   with a clean build, then doing `fx build bazel_workspace`, then
    #   `fx bazel query @gn_targets//:*` would force this repository rule
    #   to run without them.
    #
    #   Using a label attribute would fail to run the repository run entirely
    #   because Bazel would complain about missing files. The error message
    #   is extremely confusing, so instead, do a soft-detect in the rule
    #   with a better error message.
    #
    # - This repository rule should still be run every time the content of
    #   this file changes, hence the need to track them, by calling
    #   repo_ctx.path(Label("@//:<path>")), which appends their path and
    #   content hash to `$OUTPUT_BASE/external/@gn_targets.marker`, the
    #   file used by Bazel to track such content.
    #
    #
    # - These files are re-generated with different content by Ninja build
    #   actions of GN bazel_action() targets. I.e. they are mo
    #
    # - Due to this, they cannot be listed as
    #
    # These files may not exist,
    # Path to the files generated by //build/bazel/scripts/bazel_action.py
    # for each bazel_action() GN target. Note that these are string attributes
    # because the files may not exist when setting up the Bazel workspace!
    inputs_manifest_str = repo_ctx.attr.inputs_manifest
    all_licenses_spdx_json_str = repo_ctx.attr.all_licenses_spdx_json

    inputs_manifest_path = repo_ctx.workspace_root.get_child(inputs_manifest_str)
    all_licenses_spdx_path = repo_ctx.workspace_root.get_child(all_licenses_spdx_json_str)

    # The file may not exist if this rule is run manually before any
    if not inputs_manifest_path.exists:
        fail("Missing generated file: %s" % inputs_manifest_path)
    if not all_licenses_spdx_path.exists:
        fail("Missing generated file: %s" % all_licenses_spdx_path)

    # Ensure that this repository rule is re-run every time the content of
    # these files changes.
    repo_ctx.path(Label("@//:" + inputs_manifest_str))
    repo_ctx.path(Label("@//:" + all_licenses_spdx_json_str))

    # The Ninja output directory is passed by the launcher script at
    # $BAZEL_TOPDIR/bazel as an environment variable.
    #
    # This is the root directory for all source entries in the manifest.
    # Create a //:ninja_output symlink in the repository to point to it.
    ninja_output_dir = repo_ctx.os.environ["BAZEL_FUCHSIA_NINJA_OUTPUT_DIR"]
    source_prefix = ninja_output_dir + "/"

    # The top-level directory that will contain symlinks to all Ninja output
    # files. For example //_ninja_build_dir:obj/src/foo/foo.cc.o
    build_dir_name = "_files"

    all_files = []
    all_dir_links = []

    # Build a { bazel_package -> { gn_target_name -> entry } } map.
    package_map = {}
    for entry in json.decode(repo_ctx.read(inputs_manifest_path)):
        bazel_package = entry["bazel_package"]
        bazel_name = entry["bazel_name"]
        name_map = package_map.setdefault(bazel_package, {})
        name_map[bazel_name] = entry

    # Create the //targets/{gn_dir}/BUILD.bazel file for each GN directory.
    # Every target defined in {gn_dir}/BUILD.gn that is part of the manifest
    # will have its own filegroup() entry with the corresponding target name.
    for bazel_package, name_map in package_map.items():
        content = """# AUTO-GENERATED - DO NOT EDIT

package(
    default_applicable_licenses = ["//:all_licenses_spdx_json"],
    default_visibility = ["//visibility:public"],
)

"""
        dir_names = []
        for bazel_name, entry in name_map.items():
            file_links = entry.get("output_files", [])
            for file in file_links:
                target_path = source_prefix + file
                link_path = "%s/%s" % (build_dir_name, file)

                # Create //_files/{ninja_path} as a symlink to the Ninja output location.
                repo_ctx.symlink(target_path, link_path)
                all_files.append(file)

                content += '''
# From GN target: {label}
filegroup(
    name = "{name}",
    srcs = '''.format(label = entry["generator_label"], name = bazel_name)
                if len(file_links) == 1:
                    content += '["_files/%s"],\n' % file_links[0]
                else:
                    content += "[\n"
                    for file in file_links:
                        content += '        "_files/%s",\n' % file
                    content += "    ],\n"
                content += ")\n"

            dir_link = entry.get("output_directory", "")
            if dir_link:
                target_path = source_prefix + dir_link
                link_path = "%s/%s" % (build_dir_name, dir_link)

                content += '''
# From GN target: {label}
filegroup(
    name = "{name}",
    srcs = glob(["{ninja_path}/**"], exclude_directories=1),
)
'''.format(label = entry["generator_label"], name = bazel_name, ninja_path = link_path)

                # Create //_files/{ninja_path} as a symlink to the real path.
                repo_ctx.symlink(target_path, link_path)

                # Create //{gn_dir}/{bazel_name}.directory as a symlink to //_files/{ninja_path}
                repo_ctx.symlink(link_path, "%s/%s.directory" % (bazel_package, bazel_name))
                dir_names.append(bazel_name + ".directory")

        if dir_names:
            content += "exports_files([\n"
            for dir in dir_names:
                content += "   \"%s\",\n" % dir
            content += "])\n"

        repo_ctx.file("%s/BUILD.bazel" % bazel_package, content, executable = False)
        repo_ctx.symlink(build_dir_name, "%s/_files" % bazel_package)

    # The symlink for the special all_licenses_spdx.json file.
    # IMPORTANT: This must end in `.spdx.json` for license classification to work correctly!
    repo_ctx.symlink(all_licenses_spdx_path, "all_licenses.spdx.json")

    # The content of BUILD.bazel
    build_content = '''# AUTO-GENERATED - DO NOT EDIT
load("@rules_license//rules:license.bzl", "license")

# This contains information about all the licenses of all
# Ninja outputs exposed in this repository.
# IMPORTANT: package_name *must* be "Legacy Ninja Build Outputs"
# as several license pipeline exception files hard-code this under //vendor/...
license(
    name = "all_licenses_spdx_json",
    package_name = "Legacy Ninja Build Outputs",
    license_text = "all_licenses.spdx.json",
    visibility = ["//visibility:public"]
)

'''
    repo_ctx.file("BUILD.bazel", build_content)

    # Workspace declaration (required but unused in practice)
    repo_ctx.file("WORKSPACE.bazel", 'workspace(name = "{name}\n")'.format(name = repo_ctx.attr.name))
    repo_ctx.file("MODULE.bazel", 'module(name = "{name}", version = "1"),\n'.format(name = repo_ctx.attr.name))

gn_targets_repository = repository_rule(
    implementation = _gn_targets_repository_impl,
    doc = "A repository exposing Ninja outputs with Bazel filegroups.",
    attrs = {
        "inputs_manifest": attr.string(
            mandatory = True,
            doc = "Path to input manifest file, relative to workspace root.",
        ),
        "all_licenses_spdx_json": attr.string(
            mandatory = True,
            doc = "Path to SPDX file containing all license information, relative to workspace root.",
        ),
    },
)

################################################################################
################################################################################
#####
#####    googletest_repository()
#####

def _googletest_repository_impl(repo_ctx):
    """Create a @com_google_googletest repository that supports Fuchsia."""
    workspace_dir = str(repo_ctx.workspace_root)

    # IMPORTANT: keep this function in sync with the computation of
    # generated_repository_inputs['com_google_googletest'] in
    # //build/bazel/update-workspace.py.
    if hasattr(repo_ctx.attr, "content_hash_file"):
        repo_ctx.path(workspace_dir + "/" + repo_ctx.attr.content_hash_file)

    # This uses a git bundle to ensure that we can always work from a
    # Jiri-managed clone of //third_party/googletest/src/. This is more reliable
    # than the previous approach that relied on patching.
    repo_ctx.execute(
        [
            repo_ctx.path(workspace_dir + "/build/bazel/scripts/git-clone-then-apply-bundle.py"),
            "--dst-dir",
            ".",
            "--git-url",
            repo_ctx.path(workspace_dir + "/third_party/googletest/src"),
            "--git-bundle",
            repo_ctx.path(workspace_dir + "/build/bazel/patches/googletest/fuchsia-support.bundle"),
            "--git-bundle-head",
            "fuchsia-support",
        ],
        quiet = False,  # False for debugging.
    )

googletest_repository = repository_rule(
    implementation = _googletest_repository_impl,
    doc = "A repository rule used to create a googletest repository that " +
          "properly supports Fuchsia through local patching.",
    attrs = {
        "content_hash_file": attr.string(
            doc = "Path to content hash file for this repository, relative to workspace root.",
            mandatory = False,
        ),
    },
)

################################################################################
################################################################################
#####
#####    boringssl_repository()
#####

def _boringssl_repository_impl(repo_ctx):
    """Create a @boringssl repository."""

    workspace_dir = str(repo_ctx.workspace_root)
    dest_dir = repo_ctx.path(".")
    src_dir = repo_ctx.path(workspace_dir + "/third_party/boringssl")

    # IMPORTANT: keep this function in sync with the computation of
    # generated_repository_inputs['boringssl'] in
    # //build/bazel/update_workspace.py.
    if hasattr(repo_ctx.attr, "content_hash_file"):
        repo_ctx.path(workspace_dir + "/" + repo_ctx.attr.content_hash_file)

    # Link the contents of the repo into the bazel sandbox. We cannot use a
    # local_repository here because we need to execute the python script below
    # which generates the build file contents.
    repo_ctx.execute(
        [
            repo_ctx.path(workspace_dir + "/build/bazel/scripts/hardlink-directory.py"),
            "--fuchsia-dir",
            workspace_dir,
            src_dir,
            dest_dir,
        ],
        quiet = False,  # False for debugging.
    )

    # Copy the generated files into the workspace root
    generated_files = [
        "BUILD.generated.bzl",
        "BUILD.generated_tests.bzl",
    ]

    for generated_file in generated_files:
        content = repo_ctx.read(
            repo_ctx.path(workspace_dir + "/third_party/boringssl/" + generated_file),
        )
        repo_ctx.file(
            generated_file,
            content = content,
            executable = False,
        )

    # Add a BUILD file which exposes the cc_library target.
    repo_ctx.file("BUILD.bazel", content = repo_ctx.read(
        repo_ctx.path(workspace_dir + "/build/bazel/local_repositories/boringssl/BUILD.boringssl"),
    ), executable = False)

boringssl_repository = repository_rule(
    implementation = _boringssl_repository_impl,
    doc = "A repository rule used to create a boringssl repository that " +
          "has build files generated for Bazel.",
    attrs = {
        "content_hash_file": attr.string(
            doc = "Path to content hash file for this repository, relative to workspace root.",
            mandatory = False,
        ),
    },
)

################################################################################
################################################################################
#####
#####    fuchsia_build_info_repository()
#####

def _fuchsia_build_info_repository_impl(repo_ctx):
    # Extract labels as soon as possible.
    file_path = repo_ctx.path(repo_ctx.attr._gn_build_args_txt)
    args_json_path = repo_ctx.path(repo_ctx.attr.args)

    # Read args.json
    args = json.decode(repo_ctx.read(args_json_path))

    # Parse the list file to generate the content of args.bzl
    args_contents = """# AUTO-GENERATED BY fuchsia_build_info_repository() RULE - DO NOT EDIT

"""

    # Avoid Gerrit warnings by constructing the linting prefixes with string concatenation.
    lint_change_if_prefix = "LINT." + "IfChange("
    lint_change_then_prefix = "LINT." + "ThenChange("
    lint_change_if_start_line = -1
    pending_lines = ""
    line_count = 0
    for line in repo_ctx.read(file_path).splitlines():
        line_count += 1
        line = line.strip()

        if not line:  # Ignore empty lines
            continue

        if line[0] == "#":
            comment = line[1:].lstrip()
            if comment.startswith(lint_change_if_prefix):
                if pending_lines:
                    fail("{}:{}: Previous {} at line {} was never closed!".format(
                        file_path,
                        line_count,
                        lint_change_if_prefix,
                        lint_change_if_start_line,
                    ))
                lint_change_if_start_line = line_count
                continue

            if comment.startswith(lint_change_then_prefix):
                source_start = len(lint_change_then_prefix)
                source_end = comment.find(")", source_start)
                if source_end < 0:
                    fail("{}:{}: Unterminated {} line: {}".format(
                        file_path,
                        line_count,
                        lint_change_then_prefix,
                        line,
                    ))
                source_path = comment[source_start:source_end]
                args_contents += "# From {}\n".format(source_path) + pending_lines + "\n"
                pending_lines = ""
                continue

            # Skip other comment lines.
            continue

        name_end = line.find(":")
        if name_end < 0:
            fail("{}:{}: Missing colon separator: {}".format(file_path, line_count, line))

        varname = line[0:name_end]
        vartype = line[name_end + 1:].strip()
        if vartype == "bool":
            value = args.get(varname, False)
            pending_lines += "{} = {}\n".format(varname, value)
        elif vartype == "string":
            value = args.get(varname, "")
            pending_lines += "{} = \"{}\"\n".format(varname, value)
        elif vartype == "string_or_false":
            if not args.get(varname):
                value = ""
            else:
                value = args[varname]
            pending_lines += "{} = \"{}\"\n".format(varname, value)
        else:
            fail("{}:{}: Unknown type name '{}': {}".format(
                file_path,
                line_count,
                vartype,
                line,
            ))

    if pending_lines:
        fail("{}:{}: {} statement was never closed!".format(
            file_path,
            line_count,
            lint_change_if_prefix,
            lint_change_if_start_line,
        ))

    repo_ctx.file("WORKSPACE.bazel", "workspace(name = {})\n".format(repo_ctx.name))
    repo_ctx.file("BUILD.bazel", "")
    repo_ctx.file("args.bzl", args_contents)

fuchsia_build_info_repository = repository_rule(
    implementation = _fuchsia_build_info_repository_impl,
    doc = "A repository holding information about the current Fuchsia build configuration.",
    attrs = {
        "args": attr.label(
            doc = "args.json source file label.",
            allow_single_file = True,
            mandatory = True,
        ),
        "_gn_build_args_txt": attr.label(
            doc = "Input file used to list all imported GN build arguments.",
            allow_single_file = True,
            default = "//:build/bazel/gn_build_args.txt",
        ),
    },
)
