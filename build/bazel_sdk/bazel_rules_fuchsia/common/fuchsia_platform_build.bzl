# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Misc utilities that are related to the Fuchsia platform build only."""

load(
    "//common:repository_utils.bzl",
    "get_fuchsia_host_arch",
    "get_fuchsia_host_os",
)
load(
    "//common:toolchains/clang/clang_utils.bzl",
    "to_clang_target_tuple",
)

def _get_fuchsia_source_relative_path(repo_ctx, path):
    """Return the path of a given file relative to the fuchsia source directory.

    Args:
       repo_ctx: repository context
       path: A path, relative to the fuchsia source directory.

    Returns:
       A new repo_ctx.path() object pointing to the file.
       If repo_ctx.attr.fuchsia_source_dir is empty, this assumes that this
       is run from the Fuchsia platform build, and will record a dependency
       to the resulting file path, to ensure the repository rule is always
       re-run when the file changes.
    """
    fuchsia_source_dir = repo_ctx.attr.fuchsia_source_dir
    if not fuchsia_source_dir:
        return repo_ctx.path(Label("@//:" + path))

    if not fuchsia_source_dir.startswith("/"):
        fuchsia_source_dir = "%s/%s" % (repo_ctx.workspace_root, fuchsia_source_dir)

    return "%s/%s" % (fuchsia_source_dir, path)

def _get_ninja_output_dir(repo_ctx):
    """Compute the Ninja output directory used by this workspace.

    Args:
        repo_ctx: the repository context.
    Returns:
        a string containing the path to the ninja output directory,
        relative to the current workspace root.
    """

    # The config file at this location contains the location of the TOPDIR
    # used by update-workspace.py, e.g. `gen/build/bazel`. This code assumes
    # that the workspace is one of its sub-directories, so compute a
    # back-tracking path prefix by counting the number of path num_fragment
    # in it.
    config_path = _get_fuchsia_source_relative_path(
        repo_ctx,
        "build/bazel/config/main_workspace_top_dir",
    )
    top_dir = repo_ctx.read(config_path).strip()
    num_fragments = len(top_dir.split("/"))
    result = ".."
    for _ in range(num_fragments):
        result += "/.."

    if repo_ctx.attr.fuchsia_source_dir:
        result = "%s/%s" % (repo_ctx.attr.fuchsia_source_dir, result)

    # If a build.ninja file does not exist in the resulting file, something
    # is wrong, so return an invalid value that will clarify the issue in
    # future error messages if something tries to use it.
    # Do not fail() because the resulting `build_config` can be used OOT,
    # for example when invoking Bazel directly from //build/bazel_sdk/tests/
    if not repo_ctx.path(result).exists:
        result = "COULD_NOT_FIND_NINJA_OUTPUT_DIRECTORY"

    return result

def _cfg_values_to_dict(values_text):
    """Parse comma-separated key=value pairs into a dictionary."""
    values = {}
    for var in values_text.split(","):
        k, _, v = var.partition("=")
        values[k] = v
    return values

def _get_rbe_config(repo_ctx):
    """Compute RBE-related configuration.

    Args:
      repo_ctx: Repository context.
    Returns:
      A struct with fields describing RBE-related config information.
    """
    rewrapper_config_path = _get_fuchsia_source_relative_path(
        repo_ctx,
        "build/rbe/fuchsia-rewrapper.cfg",
    )

    reproxy_config_path = _get_fuchsia_source_relative_path(
        repo_ctx,
        "build/rbe/fuchsia-reproxy.cfg",
    )

    instance_prefix = "instance="
    platform_prefix = "platform="

    platform_values = {}
    for line in repo_ctx.read(rewrapper_config_path).splitlines():
        line = line.strip()
        if line.startswith(platform_prefix):
            # After "platform=", expect comma-separated key=value pairs.
            platform_values = _cfg_values_to_dict(line.removeprefix(platform_prefix))

    container_image = platform_values.get("container-image")
    gce_machine_type = platform_values.get("gceMachineType")

    instance_name = None
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
        gce_machine_type = gce_machine_type,
    )

def _get_formatted_starlark_dict(dict_value, margin):
    """Convert dictionary into formatted Starlak expression.

    Args:
        dict_value: (dict) dictionary value.
        margin: (string) A string of spaces to use as the current left-margin
           for the location where the value will be inserted.
    Returns:
        A string holding a formatted Starlark expression for the input
        dictionary. If not empty, this will use multiple lines (one per key)
        with indentation of |len(margin) + 4| spaces.
    """
    return json.encode_indent(dict_value, prefix = margin, indent = margin + "   ")

def _fuchsia_build_config_repository_impl(repo_ctx):
    host_os = get_fuchsia_host_os(repo_ctx)
    host_arch = get_fuchsia_host_arch(repo_ctx)
    host_target_triple = to_clang_target_tuple(host_os, host_arch)
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

    ninja_output_dir = _get_ninja_output_dir(repo_ctx)

    defs_content = '''# Auto-generated DO NOT EDIT

"""Global information about this build's configuration."""

build_config = struct(
    # The host operating system, using Fuchsia conventions
    host_os = "{host_os}",

    # The host CPU architecture, using Fuchsia conventions.
    host_arch = "{host_arch}",

    # The host tag, used to separate prebuilts in the Fuchsia source tree
    # (e.g. 'linux-x64', 'mac-x64', 'mac-arm64')
    host_tag = "{host_tag}",

    # The host tag, using underscores instead of dashes.
    host_tag_alt = "{host_tag_alt}",

    # The GCC/Clang target triple for the host operating system.
    # (e.g. x86_64-unknown-gnu-linux or aarcg64-apple-darwin).
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
    rbe_container_image = "{rbe_container_image}",

    # The RBE default machine type for remote build configuration, e.g. "n2-standard-2".
    rbe_gce_machine_type = "{rbe_gce_machine_type}",

    # The path to the Ninja output directory, relative to the current
    # workspace root.
    ninja_output_dir = "{ninja_output_dir}",
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
        rbe_gce_machine_type = rbe_config.gce_machine_type,
        ninja_output_dir = ninja_output_dir,
    )

    repo_ctx.file("WORKSPACE.bazel", "")
    repo_ctx.file("defs.bzl", defs_content)

    # Generate a BUILD.bazel file that contains a platform definition using the
    # right rbe_container_image execution property for linux-x64.
    if host_tag == "linux-x64":
        exec_properties = {
            "container-image": rbe_config.container_image,
            "OSFamily": "Linux",
            "gceMachineType": rbe_config.gce_machine_type,
        }
    else:
        exec_properties = {}

    # TODO: invoke `create_rbe_exec_properties_dict` to validate keys
    # https://github.com/bazelbuild/bazel-toolchains/blob/master/rules/exec_properties/README.md
    build_bazel_content = '''# Auto-generated DO NOT EDIT

"""A host platform() with optional support for remote builds."""

platform(
    name = "host",
    constraint_values = [
        "{host_os_constraint}",
        "{host_cpu_constraint}",
    ],
    exec_properties = {exec_properties_str},
)
'''.format(
        host_os_constraint = host_os_constraint,
        host_cpu_constraint = host_cpu_constraint,
        exec_properties_str = _get_formatted_starlark_dict(exec_properties, "    "),
    )
    repo_ctx.file("BUILD.bazel", build_bazel_content)

fuchsia_build_config_repository = repository_rule(
    implementation = _fuchsia_build_config_repository_impl,
    doc = "A repository rule used to create an external repository that " +
          "contains a defs.bzl file exposing information about the current " +
          "build configuration for the Fuchsia platform build.",
    attrs = {
        "fuchsia_source_dir": attr.string(
            mandatory = False,
            doc = "Path to the Fuchsia source directory, relative to the " +
                  "current workspace.",
        ),
    },
)

### BzlMod support.

def _fuchsia_build_config_ext_impl(module_ctx):
    fuchsia_source_dir = module_ctx.os.environ.get("LOCAL_FUCHSIA_PLATFORM_BUILD", "")
    if fuchsia_source_dir:
        fuchsia_source_dir += "/../.."

    fuchsia_build_config_repository(
        name = "fuchsia_build_config",
        fuchsia_source_dir = fuchsia_source_dir,
    )

fuchsia_build_config_ext = module_extension(
    implementation = _fuchsia_build_config_ext_impl,
)
