# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Defines a WORKSPACE rule for loading a version of the Fuchsia IDK."""

load("//fuchsia/workspace:utils.bzl", "symlink_or_copy", "workspace_path")
load(
    "//fuchsia/workspace/sdk_templates:generate_sdk_build_rules.bzl",
    "generate_sdk_repository",
    "resolve_repository_labels",
)

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

_LOCAL_FUCHSIA_IDK_DIRECTORY = "LOCAL_FUCHSIA_IDK_DIRECTORY"

def _instantiate_local_path(ctx, manifests):
    local_paths = ctx.attr.local_paths
    for local_path in local_paths:
        # Copies the SDK from a local Fuchsia platform build.
        if local_path[0] == "@":
            # Assume this is a file path inside the repository, e.g. @fuchsia_idk//:BUILD.bazel
            local_sdk = ctx.path(Label(local_path)).dirname
            ctx.report_progress("Copying local IDK from %s" % local_sdk)
        else:
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
    local_fuchsia_dir = ctx.os.environ.get(_LOCAL_FUCHSIA_PLATFORM_BUILD)

    # buildifier: disable=print
    print("WARNING: using local SDK from %s" % local_fuchsia_dir)
    local_sdk = ctx.path("%s/%s" % (local_fuchsia_dir, _LOCAL_BUILD_SDK_PATH))
    ctx.report_progress("Copying local fuchsia_sdk from %s" % local_fuchsia_dir)
    if not local_sdk.exists:
        fail("Cannot find SDK in local Fuchsia build.Please build it with\n\n\t\t'fx build generate_fuchsia_sdk_repository'\n\nor unset variable %s: %s" % (_LOCAL_FUCHSIA_PLATFORM_BUILD, local_fuchsia_dir))
    ctx.symlink(local_sdk, ".")

def _instantiate_local_idk(ctx, manifests):
    local_fuchsia_idk_dir = ctx.os.environ.get(_LOCAL_FUCHSIA_IDK_DIRECTORY)

    # buildifier: disable=print
    print("WARNING: using local IDK from %s" % local_fuchsia_idk_dir)
    ctx.report_progress("Copying local IDK from %s" % local_fuchsia_idk_dir)
    local_idk = ctx.path(local_fuchsia_idk_dir)
    if not local_idk.exists:
        fail("Cannot find IDK in: %s\n" % local_idk)
    manifests.append({"root": "%s/." % local_idk, "manifest": "meta/manifest.json"})

def _get_starlark_runtime_for(ctx, copy_content_strategy):
    """Return a runtime value usable with generate_sdk_build_rules.bzl functions.

    See the technical note at the start of said script describing what the result
    should look like.

    Args:
       ctx: The current repository_ctx value.
       copy_content_strategy: Argument to symlink_or_copy()
    Returns:
       A new struct that can be used as the first argument to most
       functions provided by generate_sdk_build_rules.bzl.
    """

    def _workspace_path(path):
        if path.startswith("@"):
            # This must point to a file inside the directory of interest.
            # e.g. @<repo_name>//:BUILD.bazel --> path to external/repo_name
            # This is necessary for parent_sdk_local_paths handling.
            return ctx.path(Label(path)).dirname

        return ctx.path(workspace_path(ctx, path))

    def _label_to_path(label):
        return ctx.path(Label(label))

    def _file_copier(files_to_copy):
        symlink_or_copy(ctx, copy_content_strategy, files_to_copy)

    def _json_decode(json_str):
        return json.decode(json_str)

    def _value_is_dict(v):
        return type(v) == "dict"

    def _value_is_list(v):
        return type(v) == "list"

    def _value_is_struct(v):
        return type(v) == "struct"

    def _find_repository_files_by_name(file_name):
        ret = ctx.execute(["find", "-L", ".", "-type", "f", "-name", file_name])
        if ret.return_code:
            return []
        return ret.stdout.strip().split("\n")

    def _run_buildifier(buildifier_flags):
        ret = ctx.execute(
            [str(ctx.path(ctx.attr.buildifier))] + buildifier_flags,
            quiet = False,
        )
        return ret.return_code == 0

    return struct(
        ctx = ctx,
        make_struct = struct,
        fail = fail,
        value_is_dict = _value_is_dict,
        value_is_list = _value_is_list,
        value_is_struct = _value_is_struct,
        workspace_path = _workspace_path,
        label_to_path = _label_to_path,
        file_copier = _file_copier,
        json_decode = _json_decode,
        find_repository_files_by_name = _find_repository_files_by_name,
        run_buildifier = _run_buildifier if ctx.attr.buildifier else None,
    )

def _fuchsia_sdk_repository_impl(ctx):
    if _LOCAL_FUCHSIA_PLATFORM_BUILD in ctx.os.environ:
        copy_content_strategy = "copy"
        _instantiate_local_env(ctx)
        return

    # Compute the set of input IDK manifests to use for this repository.
    manifests = []

    if _LOCAL_FUCHSIA_IDK_DIRECTORY in ctx.os.environ:
        copy_content_strategy = "symlink"
        _instantiate_local_idk(ctx, manifests)
    elif ctx.attr.local_paths:
        copy_content_strategy = "symlink"
        _instantiate_local_path(ctx, manifests)
    else:
        fail("The fuchsia sdk no longer supports downloading content via the cipd tool. Please use local_paths or provide a local fuchsia build.")

    runtime = _get_starlark_runtime_for(ctx, copy_content_strategy)

    # Resolve labels early to avoid repository rule restarts.
    resolve_repository_labels(runtime)

    generate_sdk_repository(runtime, manifests)

fuchsia_sdk_repository = repository_rule(
    doc = """
Loads a particular version of the Fuchsia IDK.
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
            doc = "Paths to local SDK directories.",
        ),
        "local_sdk_version_file": attr.label(
            doc = "An optional file used to mark the version of the SDK pointed to by local_paths.",
            allow_single_file = True,
        ),
        "buildifier": attr.label(
            doc = "An optional label to the buildifier tool, used to reformat all generated Bazel files.",
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
