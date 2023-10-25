# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
A set of repository rules used by the Bazel workspace for the Fuchsia
platform build.
"""

load(
    "//:build/bazel/repository_utils.bzl",
    "get_clang_target_triple",
    "get_fuchsia_host_arch",
    "get_fuchsia_host_os",
    "workspace_root_path",
)

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
    workspace_dir = str(workspace_root_path(repo_ctx))

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

def _get_rbe_config(repo_ctx):
    rewrapper_config_path = repo_ctx.path(Label("@//:build/rbe/fuchsia-rewrapper.cfg"))
    reproxy_config_path = repo_ctx.path(Label("@//:build/rbe/fuchsia-reproxy.cfg"))

    instance_prefix = "instance="

    # Note: platform value is a comma-separated list of key=values.
    # This extraction assumes that "container-image" is the only key-value.
    # If ever there are multiple key-values, this extraction will require
    # a little more parsing to be more robust.
    container_image_prefix = "platform=container-image="

    instance_name = None
    container_image = None

    for line in repo_ctx.read(rewrapper_config_path).splitlines():
        line = line.strip()
        if line.startswith(container_image_prefix):
            container_image = line[len(container_image_prefix):]

    for line in repo_ctx.read(reproxy_config_path).splitlines():
        line = line.strip()
        if line.startswith(instance_prefix):
            instance_name = line[len(instance_prefix):]

    if not instance_name:
        fail("ERROR: Missing instance name from %s" % reproxy_config_path)

    if not container_image:
        fail("ERROR: Missing container image name from %s" % rewrapper_config_path)

    return struct(
        instance_name = instance_name,
        container_image = container_image,
    )

def _fuchsia_build_config_repository_impl(repo_ctx):
    host_os = get_fuchsia_host_os(repo_ctx)
    host_arch = get_fuchsia_host_arch(repo_ctx)
    host_target_triple = get_clang_target_triple(host_os, host_arch)
    host_tag = "%s-%s" % (host_os, host_arch)
    host_tag_alt = "%s_%s" % (host_os, host_arch)

    host_os_constraint = "@platforms//os:" + {
        "mac": "macos",
    }.get(host_os, host_os)

    host_cpu_constraint = "@platforms//cpu:" + {
        "x64": "x86_64",
        "arm64": "aarch64",
    }.get(host_arch, host_arch)

    rbe_config = _get_rbe_config(repo_ctx)

    defs_content = '''# Auto-generated DO NOT EDIT

build_config = struct(
    # The host operating system, using Fuchsia conventions
    host_os = "{host_os}",

    # The host CPU architecture, isong Fuchsia conventions.
    host_arch = "{host_arch}",

    # The host tag, used to separate prebuilts in the Fuchsia source tree
    # (e.g. 'linux-x64', 'mac-x64', 'mac-arm64')
    host_tag = "{host_tag}",

    # The host tag, using underscores instead of dashes.
    host_tag_alt = "{host_tag_alt}",

    # The GCC/Clang target triple for the host operating system.
    # (e.g. x86_64-unknown-linux-gnu or aarch64-apple-darwin).
    host_target_triple = "{host_target_triple}",

    # The Bazel platform os constraint value for the host
    # (e.g. '@platforms//os:linux' or '@platforms//os:macos'
    host_platform_os_constraint = "{host_os_constraint}",

    # The Bazel platform cpu constraint value for the host
    # (e.g. '@platforms//cpu:x86_64' or '@platforms//cpu:aarch64')
    host_platform_cpu_constraint = "{host_cpu_constraint}",

    # The RBE instance name for remote build configuration. Empty if disabled.
    rbe_instance_name = "{rbe_instance_name}",

    # The RBE container image for remote build configuration. Empty if disabled.
    rbe_container_image = "{rbe_container_image}"
)
'''.format(
        host_os = host_os,
        host_arch = host_arch,
        host_target_triple = host_target_triple,
        host_tag = host_tag,
        host_tag_alt = host_tag_alt,
        host_os_constraint = host_os_constraint,
        host_cpu_constraint = host_cpu_constraint,
        rbe_instance_name = rbe_config.instance_name,
        rbe_container_image = rbe_config.container_image,
    )

    repo_ctx.file("WORKSPACE.bazel", "")
    repo_ctx.file("BUILD.bazel", "")
    repo_ctx.file("defs.bzl", defs_content)

fuchsia_build_config_repository = repository_rule(
    implementation = _fuchsia_build_config_repository_impl,
    doc = "A repository rule used to create an external repository that " +
          "contains a defs.bzl file exposing information about the current " +
          "build configuration for the Fuchsia platform build.",
)
