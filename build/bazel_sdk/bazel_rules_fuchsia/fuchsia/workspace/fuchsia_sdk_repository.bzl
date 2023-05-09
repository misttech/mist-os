# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Defines a WORKSPACE rule for loading a version of the Fuchsia IDK."""

load("//fuchsia/workspace/sdk_templates:generate_sdk_build_rules.bzl", "generate_sdk_build_rules", "generate_sdk_constants", "sdk_id_from_manifests")
load("//fuchsia/workspace:utils.bzl", "workspace_path")

# Environment variable used to set a local Fuchsia Platform tree build output
# directory. If this variable is set, it should point to
# <FUCHSIA_DIR>/out/<BUILD_DIR> where "fuchsia_sdk" is built. In particular we
# will look for
#
#     <LOCAL_FUCHSIA_PLATFORM_BUILD>/gen/build/bazel/fuchsia_sdk
#
# This can be produced with a 'fx build generate_fuchsia_sdk_repository' command
# in a Fuchsia Platform tree.

_LOCAL_FUCHSIA_PLATFORM_BUILD = "LOCAL_FUCHSIA_PLATFORM_BUILD"
_LOCAL_BUILD_SDK_PATH = "gen/build/bazel/fuchsia_sdk"

def _instantiate_local_path(ctx, manifests):
    local_paths = []
    if ctx.attr.local_paths:
        local_paths.extend(ctx.attr.local_paths)

    for local_path in local_paths:
        # Copies the SDK from a local Fuchsia platform build.
        local_sdk_path = workspace_path(ctx, local_path)
        ctx.report_progress("Copying local SDK from %s" % local_sdk_path)
        local_sdk = ctx.path(local_sdk_path)
        if not local_sdk.exists:
            fail("Cannot find SDK in local Fuchsia build: %s\n\nPlease build it with\n\n\t\t'fx build generate_fuchsia_sdk_repository'" % local_sdk)

        manifests.append({"root": "%s/." % local_sdk, "manifest": "meta/manifest.json"})

    # If local_sdk_version_file is specified, make Bazel pick it up as a dep.
    if ctx.attr.local_sdk_version_file:
        ctx.path(ctx.attr.local_sdk_version_file)

def _instantiate_local_env(ctx):
    # Copies the fuchsia_sdk from a local Fuchsia platform build.
    local_fuchsia_dir = ctx.os.environ[_LOCAL_FUCHSIA_PLATFORM_BUILD]

    # buildifier: disable=print
    print("WARNING: using local SDK from %s" % local_fuchsia_dir)
    ctx.report_progress("Copying local fuchsia_sdk from %s" % local_fuchsia_dir)
    local_sdk = ctx.path("%s/%s" % (local_fuchsia_dir, _LOCAL_BUILD_SDK_PATH))
    if not local_sdk.exists:
        fail("Cannot find SDK in local Fuchsia build.Please build it with\n\n\t\t'fx build generate_fuchsia_sdk_repository'\n\nor unset variable %s: %s" % (_LOCAL_FUCHSIA_PLATFORM_BUILD, local_fuchsia_dir))
    ctx.symlink(local_sdk, ".")

def _merge_rules_fuchsia(ctx):
    rules_fuchsia_root = ctx.path(ctx.attr._rules_fuchsia_root).dirname
    ctx.symlink(rules_fuchsia_root.get_child("fuchsia"), "fuchsia")

    rules_fuchsia_build = ctx.read(rules_fuchsia_root.get_child("BUILD.bazel")).split("\n")
    start, end = [
        i
        for i, s in enumerate(rules_fuchsia_build)
        if "__BEGIN_FUCHSIA_SDK_INCLUDE__" in s or "__END_FUCHSIA_SDK_INCLUDE__" in s
    ]
    rules_fuchsia_build_fragment = "\n".join(rules_fuchsia_build[start:end + 1])
    ctx.template(
        "BUILD.bazel",
        "BUILD.bazel",
        substitutions = {
            "{{__FUCHSIA_SDK_INCLUDE__}}": rules_fuchsia_build_fragment,
        },
    )

def _fuchsia_sdk_repository_impl(ctx):
    if _LOCAL_FUCHSIA_PLATFORM_BUILD in ctx.os.environ:
        copy_content_strategy = "copy"
        _instantiate_local_env(ctx)
        return

    if not ctx.attr.local_paths:
        fail("The fuchsia sdk no longer supports downloading content via the cipd tool. Please use local_paths or provide a local fuchsia build.")

    ctx.file("WORKSPACE.bazel", content = "")
    manifests = []
    copy_content_strategy = "symlink"
    _instantiate_local_path(ctx, manifests)

    ctx.report_progress("Generating Bazel rules for the SDK")
    ctx.template(
        "BUILD.bazel",
        ctx.attr._template,
        substitutions = {
            "{{SDK_ID}}": sdk_id_from_manifests(ctx, manifests),
        },
    )

    # Extract the target CPU names supported by our SDK manifests, then
    # write it to generated_constants.bzl file.
    constants = generate_sdk_constants(ctx, manifests)

    # TODO(fxbug.dev/117511): Allow generate_sdk_build_rules to provide
    # substitutions directly to the call to ctx.template above.
    generate_sdk_build_rules(ctx, manifests, copy_content_strategy, constants)

    _merge_rules_fuchsia(ctx)

fuchsia_sdk_repository = repository_rule(
    doc = """
Loads a particular version of the Fuchsia IDK.

If cipd_tag is set, sha256 can optionally be set to verify the downloaded file and to
allow Bazel to cache the file.
""",
    implementation = _fuchsia_sdk_repository_impl,
    environ = [_LOCAL_FUCHSIA_PLATFORM_BUILD],
    configure = True,
    attrs = {
        "parent_sdk": attr.label(
            doc =
                """
                If specified, libraries in current SDK that also exist in the parent SDK will always resolve to the parent. In practice,
                this means that a library defined in the current SDK that is also defined in parent_sdk will be ignored in the current SDK,
                and references to it will be replaced with @<parent_sdk>//<library>. This is useful when SDKs are layered, for example an
                internal SDK and a public SDK.
                """,
            mandatory = False,
        ),
        "parent_sdk_local_paths": attr.string_list(
            doc =
                """
                If parent_sdk is specified, parent_sdk_local_paths has to contain the same values as the local_paths attribute of the parent SDK.
                This is required because Bazel does not have a way to evaluate the existance of a Label, so we process the metadata of the parent
                SDK again when using layered SDKs.
                TODO: look for a better approach if this is limiting or causing performance issues.
                """,
            mandatory = False,
        ),
        "local_paths": attr.string_list(
            doc = "Paths to local SDK directories. Incompatible with 'cipd_tag'.",
        ),
        "local_sdk_version_file": attr.label(
            doc = "An optional file used to mark the version of the SDK pointed to by local_paths.",
            allow_single_file = True,
        ),
        "fuchsia_api_level_override": attr.string(
            doc = "API level override to use when building Fuchsia.",
        ),
        "_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:repository_template.BUILD",
            allow_single_file = True,
        ),
        # The templates need to be explicitly declared so that they trigger a new
        # build when they change.
        "_bind_library_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:bind_library_template.BUILD",
            allow_single_file = True,
        ),
        "_cc_library_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:cc_library_template.BUILD",
            allow_single_file = True,
        ),
        "_cc_prebuilt_library_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:cc_prebuilt_library_template.BUILD",
            allow_single_file = True,
        ),
        "_cc_prebuilt_library_distlib_subtemplate": attr.label(
            default = "//fuchsia/workspace/sdk_templates:cc_prebuilt_library_distlib_subtemplate.BUILD",
            allow_single_file = True,
        ),
        "_cc_prebuilt_library_linklib_subtemplate": attr.label(
            default = "//fuchsia/workspace/sdk_templates:cc_prebuilt_library_linklib_subtemplate.BUILD",
            allow_single_file = True,
        ),
        "_component_manifest_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:component_manifest_template.BUILD",
            allow_single_file = True,
        ),
        "_component_manifest_collection_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:component_manifest_collection_template.BUILD",
            allow_single_file = True,
        ),
        "_ffx_tool_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:ffx_tool_template.BUILD",
            allow_single_file = True,
        ),
        "_fidl_library_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:fidl_library_template.BUILD",
            allow_single_file = True,
        ),
        "_host_tool_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:host_tool_template.BUILD",
            allow_single_file = True,
        ),
        "_companion_host_tool_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:companion_host_tool_template.BUILD",
            allow_single_file = True,
        ),
        "_api_version_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:api_version_template.bzl",
            allow_single_file = True,
        ),
        "_sysroot_template": attr.label(
            default = "//fuchsia/workspace/sdk_templates:sysroot_template.BUILD",
            allow_single_file = True,
        ),
        "_sysroot_arch_subtemplate": attr.label(
            default = "//fuchsia/workspace/sdk_templates:sysroot_arch_subtemplate.BUILD",
            allow_single_file = True,
        ),
        "_rules_fuchsia_root": attr.label(
            default = "//:BUILD.bazel",
            allow_single_file = True,
        ),
    },
)

def _fuchsia_sdk_repository_ext(ctx):
    local_paths = None

    for mod in ctx.modules:
        # only the root module can set tags, and only one
        # of version() or local() tag can be used.
        if mod.is_root and len(mod.tags.local) > 0:
            local_paths = []
            for p in mod.tags.local:
                if p.path:
                    local_paths.extend(p.path)

    fuchsia_sdk_repository(
        name = "fuchsia_sdk",
        local_paths = local_paths,
    )

# A tag used to specify a local Fuchsia SDK repository.
_local_tag = tag_class(
    attrs = {
        "path": attr.string(
            doc = "Path to local SDK directory, relative to the workspace root",
            mandatory = True,
        ),
    },
)

fuchsia_sdk_ext = module_extension(
    implementation = _fuchsia_sdk_repository_ext,
    tag_classes = {"local": _local_tag},
)
