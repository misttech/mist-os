# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Defines a WORKSPACE rule for loading a version of the Fuchsia IDK."""

load("//fuchsia/workspace/sdk_templates:generate_sdk_build_rules.bzl", "generate_sdk_build_rules", "generate_sdk_constants", "sdk_id_from_manifests")
load("//fuchsia/workspace:utils.bzl", "normalize_os", "workspace_path")
load("//cipd:defs.bzl", "fetch_cipd_contents")

# Base URL for Fuchsia IDK archives.
_SDK_URL_TEMPLATE = "https://chrome-infra-packages.appspot.com/dl/fuchsia/sdk/{type}/{os}-amd64/+/{tag}"

# Environment variable used to set a local Fuchsia Platform tree build output directory
# If this variable is set, it should point to <FUCHSIA_DIR>/out/<BUILD_DIR> where "sdk"
# and optionally "sdk:driver" target(s) are built. In particular we will look for
#     <LOCAL_FUCHSIA_PLATFORM_BUILD>/sdk/exported/core
# and
#     <LOCAL_FUCHSIA_PLATFORM_BUILD>/sdk/exported/driver  (if "use_experimental" is True)
# Those can be produced with a 'fx build sdk sdk:driver' command in a Fuchsia Platform tree.

_LOCAL_FUCHSIA_PLATFORM_BUILD = "LOCAL_FUCHSIA_PLATFORM_BUILD"
_LOCAL_BUILD_SDK_PATH = "sdk/exported/core"
_IDK_INTERNAL_PATH = "_idk"

def _sdk_url(os, tag, type = "core"):
    # Return the URL of the SDK given an Operating System string and
    # a CIPD tag.
    return _SDK_URL_TEMPLATE.format(os = os, tag = tag, type = type)

def _instantiate_local_path(ctx, manifests):
    local_paths = []
    if ctx.attr.local_path:
        local_paths.append(ctx.attr.local_path)
    if ctx.attr.local_paths:
        local_paths.extend(ctx.attr.local_paths)

    for local_path in local_paths:
        # Copies the SDK from a local Fuchsia platform build.
        local_sdk_path = workspace_path(ctx, local_path)
        ctx.report_progress("Copying local SDK from %s" % local_sdk_path)
        local_sdk = ctx.path(local_sdk_path)
        if not local_sdk.exists:
            fail("Cannot find SDK in local Fuchsia build: %s" % local_sdk)

        manifests.append({"root": "%s/." % local_sdk, "manifest": "meta/manifest.json"})

    # If local_sdk_version_file is specified, make Bazel pick it up as a dep.
    if ctx.attr.local_sdk_version_file:
        ctx.path(ctx.attr.local_sdk_version_file)

def _instantiate_local_env(ctx, manifests):
    # Copies the SDK from a local Fuchsia platform build.
    local_fuchsia_dir = ctx.os.environ[_LOCAL_FUCHSIA_PLATFORM_BUILD]

    # buildifier: disable=print
    print("WARNING: using local SDK from %s" % local_fuchsia_dir)
    ctx.report_progress("Copying local SDK from %s" % local_fuchsia_dir)
    local_sdk = ctx.path("%s/%s" % (local_fuchsia_dir, _LOCAL_BUILD_SDK_PATH))
    if not local_sdk.exists:
        fail("Cannot find SDK in local Fuchsia build. Please build it with 'fx build sdk' or unset variable %s: %s" % (_LOCAL_FUCHSIA_PLATFORM_BUILD, local_sdk))

    manifests.append({"root": "%s/." % local_sdk, "manifest": "meta/manifest.json"})

def _merge_rules_fuchsia(ctx):
    rules_fuchsia_root = ctx.path(ctx.attr._rules_fuchsia_root).dirname
    for child in ["cipd", "fuchsia"]:
        ctx.symlink(rules_fuchsia_root.get_child(child), child)

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
    ctx.file("WORKSPACE.bazel", content = "")
    manifests = []
    normalized_os = normalize_os(ctx)
    if ctx.attr.local_paths or ctx.attr.local_path:
        if ctx.attr.cipd_tag:
            fail("Do not set both local_paths and cipd_tag when calling fuchsia_sdk_repository()")
        copy_content_strategy = "symlink"
        _instantiate_local_path(ctx, manifests)
    elif _LOCAL_FUCHSIA_PLATFORM_BUILD in ctx.os.environ:
        copy_content_strategy = "copy"
        _instantiate_local_env(ctx, manifests)
    elif ctx.attr.cipd_tag:
        copy_content_strategy = "symlink"
        sha256 = ""
        if ctx.attr.sha256:
            sha256 = ctx.attr.sha256[normalized_os]
        ctx.download_and_extract(
            _sdk_url(normalized_os, ctx.attr.cipd_tag),
            type = "zip",
            sha256 = sha256,
            output = _IDK_INTERNAL_PATH,
        )
        manifests.append({"root": _IDK_INTERNAL_PATH, "manifest": "meta/manifest.json"})

    else:
        copy_content_strategy = "symlink"
        fetch_cipd_contents(ctx, ctx.attr._cipd_bin, ctx.attr._cipd_ensure_file, root = _IDK_INTERNAL_PATH)
        manifests.append({"root": _IDK_INTERNAL_PATH, "manifest": "meta/manifest.json"})

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
        "cipd_tag": attr.string(
            doc = "CIPD tag for the version to load.",
        ),
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
        "sha256": attr.string_dict(
            doc = "Optional SHA-256 hash of the SDK archive. Valid keys are: mac, linux",
        ),
        "local_path": attr.string(
            doc = "DO NOT USE. Soft transition, will be removed soon. Use local_paths instead.",
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
        "_cipd_ensure_file": attr.label(
            doc = "A cipd ensure file to use to download the sdk.",
            default = "//fuchsia/manifests:core_sdk.ensure",
        ),
        "_cipd_bin": attr.label(
            doc = "The cipd binary that will be used to download the sdk",
            default = "@cipd_tool//:cipd",
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
    cipd_tag = None
    sha256 = None
    local_paths = None

    for mod in ctx.modules:
        # only the root module can set tags, and only one
        # of version() or local() tag can be used.
        if mod.is_root:
            if len(mod.tags.version) == 1:
                if len(mod.tags.local) > 0:
                    fail("Cannot use both version() and local() for fuchsia_sdk_ext")
                version_info = mod.tags.version[0]
                cipd_tag = version_info.cipd_tag
                sha256 = version_info.sha256
            elif len(mod.tags.local) > 0:
                local_paths = []
                for p in mod.tags.local:
                    if p.path:
                        local_paths.extend(p.path)

    fuchsia_sdk_repository(
        name = "fuchsia_sdk",
        cipd_tag = cipd_tag,
        sha256 = sha256,
        local_paths = local_paths,
    )

# A tag used to specify a specific SDK version downloaded through CIPD.
_version_tag = tag_class(
    attrs = {
        "cipd_tag": attr.string(
            doc = "CIPD tag for the version to load.",
        ),
        "sha256": attr.string_dict(
            doc = "Optional SHA-256 hash of the SDK archive. Valid keys are: mac, linux",
        ),
    },
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
    tag_classes = {"version": _version_tag, "local": _local_tag},
)
