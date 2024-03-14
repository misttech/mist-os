# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load(":providers.bzl", "FuchsiaComponentManifestShardCollectionInfo", "FuchsiaComponentManifestShardInfo")

def _fuchsia_component_manifest_shard_collection_impl(ctx):
    return FuchsiaComponentManifestShardCollectionInfo(
        shards = [dep for dep in ctx.attr.deps],
    )

fuchsia_component_manifest_shard_collection = rule(
    doc = """Encapsulates a collection of component manifests and their include paths.

    This rule is not intended to be used directly. Rather, it should be added to the
    fuchsia sdk toolchain to be added as implicity dependencies for all manifests.
""",
    implementation = _fuchsia_component_manifest_shard_collection_impl,
    attrs = {
        "deps": attr.label_list(
            doc = "A list of component manifest shard targets to collect.",
            providers = [[FuchsiaComponentManifestShardInfo]],
        ),
    },
)

def _fuchsia_component_manifest_shard_impl(ctx):
    return [
        FuchsiaComponentManifestShardInfo(
            file = ctx.file.src,
            base_path = ctx.attr.include_path,
        ),
    ]

fuchsia_component_manifest_shard = rule(
    doc = """Encapsulates a component manifest shard from a input file.
""",
    implementation = _fuchsia_component_manifest_shard_impl,
    attrs = {
        "include_path": attr.string(
            doc = "Base path of the shard, used in includepath argument of cmc compile",
            mandatory = True,
        ),
        "src": attr.label(
            doc = "The component manifest shard",
            allow_single_file = [".cml"],
        ),
    },
)

def _fuchsia_component_manifest_impl(ctx):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    if not ctx.file.src and not ctx.attr.content:
        fail("Either 'src' or 'content' needs to be specified.")

    if ctx.file.src and ctx.attr.content:
        fail("Only one of 'src' and 'content' can be specified.")

    if ctx.file.src:
        manifest_in = ctx.file.src
    else:
        # create a manifest file from the given content
        if not ctx.attr.component_name:
            fail("Attribute 'component_name' has to be specified when using inline content.")

        manifest_in = ctx.actions.declare_file("%s.cml" % ctx.attr.component_name)
        ctx.actions.write(
            output = manifest_in,
            content = ctx.attr.content,
            is_executable = False,
        )

    # output should have the .cm extension
    manifest_out = ctx.actions.declare_file(manifest_in.basename[:-1])

    if ctx.configuration.coverage_enabled:
        coverage_shard = ctx.attr._sdk_coverage_shard[FuchsiaComponentManifestShardInfo]
        manifest_merged = ctx.actions.declare_file("%s_merged.cml" % manifest_in.basename[:-4])
        ctx.actions.run(
            executable = sdk.cmc,
            arguments = [
                "merge",
                "--output",
                manifest_merged.path,
                manifest_in.path,
                coverage_shard.file.path,
            ],
            inputs = [
                manifest_in,
                coverage_shard.file,
            ],
            outputs = [manifest_merged],
        )
        manifest_in = manifest_merged

    # use a dict to eliminate workspace root duplicates
    include_path_dict = {}
    includes = []
    for dep in ctx.attr.includes + sdk.cmc_includes[FuchsiaComponentManifestShardCollectionInfo].shards:
        if FuchsiaComponentManifestShardInfo in dep:
            shard = dep[FuchsiaComponentManifestShardInfo]
            includes.append(shard.file)
            include_path_dict[shard.file.owner.workspace_root + "/" + shard.base_path] = 1

    include_path = []
    for w in include_path_dict.keys():
        include_path.extend(["--includepath", w])

    ctx.actions.run(
        executable = sdk.cmc,
        arguments = [
            "compile",
            "--output",
            manifest_out.path,
            manifest_in.path,
            "--includeroot",
            manifest_in.path[:-len(manifest_in.basename)],
        ] + include_path,
        inputs = [manifest_in] + includes,
        outputs = [
            manifest_out,
        ],
        mnemonic = "CmcCompile",
    )

    return [
        DefaultInfo(files = depset([manifest_out])),
    ]

_fuchsia_component_manifest = rule(
    doc = """Compiles a component manifest from a input file.

This rule will compile an input cml file and output a cm file. The file can,
optionally, include additional cml files but they must be relative to the
src file and included in the includes attribute.

```
{
    include: ["foo.cml", "some_dir/bar.cml"]
}
```
""",
    implementation = _fuchsia_component_manifest_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "src": attr.label(
            doc = "The source manifest to compile",
            allow_single_file = [".cml"],
        ),
        "content": attr.string(
            doc = "Inline content for the manifest",
        ),
        "component_name": attr.string(
            doc = "Name of the component for inline manifests",
        ),
        "includes": attr.label_list(
            doc = "A list of dependencies which are included in the src cml",
            providers = [FuchsiaComponentManifestShardInfo],
        ),
        # This is to get the coverage.shard.cml in the SDK, so it can be merged
        # in when coverage is enabled.
        "_sdk_coverage_shard": attr.label(
            default = "@fuchsia_sdk//pkg/sys/testing:coverage",
        ),
    },
)

def fuchsia_component_manifest(*, name, tags = ["manual"], **kwargs):
    _fuchsia_component_manifest(
        name = name,
        tags = tags,
        **kwargs
    )
