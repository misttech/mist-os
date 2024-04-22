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
        content = '''# From GN target: {label}
filegroup(
    name = "{name}",
'''.format(label = gn_label, name = entry["name"])
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

def _fuchsia_build_info_repository_impl(repo_ctx):
    # Extract labels as soon as possible.
    file_path = repo_ctx.path(repo_ctx.attr._gn_build_args_txt)
    args_json_path = repo_ctx.path(repo_ctx.attr.args)

    # As a special case, look for files that are normally generated by Jiri hooks.
    # These may be missing if the hook is not active yet, so create a dict that
    # maps their workspace-relative paths to booleans.
    available_jiri_hook_files = {
        p: repo_ctx.path("{}/{}".format(repo_ctx.workspace_root, p)).exists
        for p in [
            # LINT.IfChange
            "build/info/jiri_generated/integration_commit_hash.txt",
            "build/info/jiri_generated/integration_commit_stamp.txt",
            # LINT.ThenChange(//build/info/info.bzl)
        ]
    }

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

    args_contents += "# Whether the following files were generated by Jiri hooks.\n"
    args_contents += "available_jiri_hook_files = {\n"

    # Sadly, using json.encode_indent() does not work because it writes 'false' instead
    # of 'False' in the output, which results in Starlark parsing errors later.
    for path, available in available_jiri_hook_files.items():
        args_contents += "    \"{}\": {},\n".format(path, available)
    args_contents += "}\n"

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
