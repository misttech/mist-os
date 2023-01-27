# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""CIPD repository rule utilities."""

load("//fuchsia/workspace:utils.bzl", "workspace_path")

_CIPD_GLOBAL_BIN_ENV = "CIPD_BIN"

def _verify_cipd_bin(repository_ctx, bin):
    exec_result = repository_ctx.execute([
        bin,
        "--version",
    ])

    if exec_result.return_code != 0:
        fail("Invalid cipd binary provided ")

def _setup_from_path(repository_ctx, path):
    bin = repository_ctx.path(path)

    if repository_ctx.attr.validate_cipd_bin:
        _verify_cipd_bin(repository_ctx, bin)

    repository_ctx.symlink(bin, "cipd")

def _platform(repository_ctx):
    os = repository_ctx.os
    name = os.name.split(" ")[0].lower()
    arch = os.arch
    if arch == "x86_64":
        arch = "amd64"
    elif arch == "aarch64":
        arch = "arm64"

    return "{}-{}".format(name, arch)

def _cipd_revision(repository_ctx):
    version_file = repository_ctx.attr._cipd_client_version
    rev = repository_ctx.read(version_file)
    return rev

def _sha_for_platform(repository_ctx, platform):
    digests_file = repository_ctx.attr._cipd_client_version_digests
    digests = repository_ctx.read(digests_file)
    for line in digests.splitlines():
        if line.startswith("#"):
            continue

        # split(" ") will include the blanks so we need to split and then
        # strip out the blanks
        parts = []
        for part in line.split(" "):
            if part != "":
                parts.append(part)
        if len(parts) == 3 and parts[0] == platform:
            return parts[2]

    return None

def _download_cipd_binary(repository_ctx):
    platform = _platform(repository_ctx)
    rev = _cipd_revision(repository_ctx)
    url = "https://chrome-infra-packages.appspot.com/client?platform={}&version={}".format(platform, rev)
    sha = _sha_for_platform(repository_ctx, platform)

    if sha == None:
        fail("unsupported cipd platform {}".format(platform))

    repository_ctx.download(
        url,
        executable = True,
        output = "cipd",
        sha256 = sha,
    )

def _cipd_tool_repository_impl(repository_ctx):
    if repository_ctx.attr.bin:
        bin_path = workspace_path(repository_ctx, repository_ctx.attr.bin)
        _setup_from_path(repository_ctx, bin_path)
    elif repository_ctx.os.environ.get(_CIPD_GLOBAL_BIN_ENV):
        _setup_from_path(
            repository_ctx,
            repository_ctx.os.environ.get(_CIPD_GLOBAL_BIN_ENV),
        )
    else:
        _download_cipd_binary(repository_ctx)

    repository_ctx.file("WORKSPACE.bazel", content = " ")
    repository_ctx.file("BUILD.bazel", content = 'exports_files(glob(["**/*"]) )')

cipd_tool_repository = repository_rule(
    implementation = _cipd_tool_repository_impl,
    attrs = {
        "bin": attr.string(doc = "The path to the cipd binary"),
        "validate_cipd_bin": attr.bool(
            doc = "Whether we should validate that the cipd binary is valid",
            default = True,
        ),
        "_cipd_client_version": attr.label(default = "//cipd/private/cipd_digests:cipd_client_version"),
        "_cipd_client_version_digests": attr.label(default = "//cipd/private/cipd_digests:cipd_client_version.digests"),
    },
)

def _cipd_tool_repository_ext_impl(ctx):
    bin = None
    for mod in ctx.modules:
        # Only allow the root module to set the binary.
        if mod.is_root:
            # Check to see if the root set the bin to use.
            client_tags = mod.tags.client
            if len(client_tags) == 2:
                fail("only one client tag may be specified at a time")
            elif len(client_tags) == 1:
                bin = client_tags[0].bin

    cipd_tool_repository(
        name = "cipd_tool",
        bin = bin,
    )

_cipd_client_tag = tag_class(attrs = {"bin": attr.string()})

cipd_tool_ext = module_extension(
    implementation = _cipd_tool_repository_ext_impl,
    tag_classes = {"client": _cipd_client_tag},
)
