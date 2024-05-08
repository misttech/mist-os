# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Defines a WORKSPACE rule for loading a version of the Fuchsia IDK."""

load("//fuchsia/workspace:utils.bzl", "workspace_path")
load("//fuchsia/workspace/sdk_templates:generate_sdk_build_rules.bzl", "generate_sdk_build_rules", "generate_sdk_constants", "resolve_repository_labels", "sdk_id_from_manifests", "serialize")

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

def _merge_rules_fuchsia(ctx):
    rules_fuchsia_root = ctx.path(Label("//:BUILD.bazel")).dirname
    ctx.symlink(rules_fuchsia_root.get_child("fuchsia"), "fuchsia")

    # Merge the doc_gen directory so we can generate our build rules
    ctx.symlink(rules_fuchsia_root.get_child("doc_gen"), "doc_gen")

    # LINT.IfChange
    # Link the common directory so that we can later use it as its own repo.
    ctx.symlink(rules_fuchsia_root.get_child("common"), "common")
    # LINT.ThenChange(rules_fuchsia_deps.bzl)

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
        executable = False,
    )

def _export_all_files(ctx):
    result = ctx.execute(["find", "-L", ".", "-type", "f", "-name", "BUILD.bazel"])
    if result.return_code:
        fail("Failed to enumerate all subpackages in @fuchsia_sdk")

    # LINT.IfChange
    EXPOSED_DEP = "_EXPORT_SUBPACKAGE_FILEGROUP"
    # LINT.ThenChange(//build/bazel_sdk/bazel_rules_fuchsia/fuchsia/workspace/sdk_templates/generate_sdk_build_rules.bzl)

    # Get a list of all BUILD.bazel files.
    all_build_files = result.stdout.strip().split("\n")

    # Filter only the ones that expose `:_EXPORT_SUBPACKAGE_FILEGROUP`.
    # Transform "./<pkg>/BUILD.bazel" to "//<pkg>:_EXPORT_SUBPACKAGE_FILEGROUP`.
    exported_targets = [
        "//%s:%s" % ("/".join(build_file.split("/")[1:-1]), EXPOSED_DEP)
        for build_file in all_build_files
        if EXPOSED_DEP in ctx.read(build_file)
    ]

    ctx.template(
        "BUILD.bazel",
        "BUILD.bazel",
        substitutions = {
            "{{ALL_FILES}}": serialize(exported_targets),
        },
        executable = False,
    )

def _fuchsia_sdk_repository_impl(ctx):
    if _LOCAL_FUCHSIA_PLATFORM_BUILD in ctx.os.environ:
        copy_content_strategy = "copy"
        _instantiate_local_env(ctx)
        return

    manifests = []

    # Resolve labels early to avoid repository rule restarts.
    repo_build_template = ctx.path(Label("//fuchsia/workspace/sdk_templates:repository.BUILD.template"))
    resolve_repository_labels(ctx)

    if _LOCAL_FUCHSIA_IDK_DIRECTORY in ctx.os.environ:
        copy_content_strategy = "symlink"
        _instantiate_local_idk(ctx, manifests)
    elif ctx.attr.local_paths:
        copy_content_strategy = "symlink"
        _instantiate_local_path(ctx, manifests)
    else:
        fail("The fuchsia sdk no longer supports downloading content via the cipd tool. Please use local_paths or provide a local fuchsia build.")

    # Extract the target CPU names supported by our SDK manifests, then
    # write it to generated_constants.bzl file.
    constants = generate_sdk_constants(ctx, manifests)

    ctx.file("WORKSPACE.bazel", content = "", executable = False)
    ctx.report_progress("Generating Bazel rules for the SDK")
    ctx.template(
        "BUILD.bazel",
        repo_build_template,
        substitutions = {
            "{{HOST_CPU}}": constants.host_cpus[0],
            "{{SDK_ID}}": sdk_id_from_manifests(ctx, manifests),
        },
        executable = False,
    )

    # TODO(https://fxbug.dev/42068729): Allow generate_sdk_build_rules to provide
    # substitutions directly to the call to ctx.template above.
    generate_sdk_build_rules(ctx, manifests, copy_content_strategy, constants)

    _merge_rules_fuchsia(ctx)

    # Should only be called after all BUILD.bazel files have been added.
    _export_all_files(ctx)

    # Run buildifier on all generated files, if the host tool is provided.
    if ctx.attr.buildifier:
        # First call with -lint=fix to automatically correct most issues.
        ret = ctx.execute(
            [
                str(ctx.path(ctx.attr.buildifier)),
                "-mode=fix",
                "-lint=fix",
                "-r",
                ".",
            ],
            quiet = False,
        )
        if ret.return_code != 0:
            fail("Error reformating Bazel SDK files!")

        # Second call with -lint=warn to verify that there aren't any remaining
        # issues that couldn't be fixed previously. This happens for warnings like
        # module-docstring, or bzl-visibility which require manual fixes.
        ret = ctx.execute(
            [
                str(ctx.path(ctx.attr.buildifier)),
                "-mode=fix",
                "-lint=warn",
                "-r",
                ".",
            ],
            quiet = False,
        )
        if ret.return_code != 0:
            fail("Bazel formatting errors persist in Bazel SDK files!")

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
        "fuchsia_api_level_override": attr.string(
            doc = "API level override to use when building Fuchsia.",
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
