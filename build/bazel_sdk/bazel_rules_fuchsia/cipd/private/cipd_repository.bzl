# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""CIPD repository rule and related functions."""

load(":cipd_tool.bzl", "cipd_tool_repository")
load("@bazel_tools//tools/build_defs/repo:utils.bzl", "maybe", "workspace_and_buildfile")

# Base URL for CIPD archives.
_CIPD_URL_TEMPLATE = "https://chrome-infra-packages.appspot.com/dl/{package_path}/{os}-{arch}/+/{tag}"

def cipd_repository(name, ensure_file, **kwargs):
    # Verify that we have a cipd tool repo
    maybe(
        name = "cipd_tool",
        repo_rule = cipd_tool_repository,
    )
    _cipd_repository(name = name, ensure_file = ensure_file, **kwargs)

def fetch_cipd_contents(repository_ctx, cipd_bin, ensure_file, root = "."):
    result = repository_ctx.execute(
        [
            repository_ctx.path(cipd_bin),
            "ensure",
            "-ensure-file",
            repository_ctx.path(ensure_file),
            "-root",
            root,
            "-max-threads=0",
        ],
    )
    if result.return_code != 0:
        fail("Unable to download cipd content for {}\n{}".format(ensure_file, result.stderr))

def _https_url(package_path, os, arch, tag):
    return _CIPD_URL_TEMPLATE.format(package_path = package_path, os = os, arch = arch, tag = tag)

def fetch_cipd_contents_from_https(repository_ctx, package_path, os, arch, tag, sha256 = "", root = "."):
    result = repository_ctx.download_and_extract(
        _https_url(package_path, os, arch, tag),
        type = "zip",
        sha256 = sha256,
        output = root,
    )

    return result.sha256

def _cipd_repository_impl(repository_ctx):
    fetch_cipd_contents(repository_ctx, repository_ctx.attr.cipd_bin, repository_ctx.attr.ensure_file)
    workspace_and_buildfile(repository_ctx)

_cipd_repository = repository_rule(
    implementation = _cipd_repository_impl,
    attrs = {
        "ensure_file": attr.label(allow_files = True),
        "cipd_bin": attr.label(default = "@cipd_tool//:cipd"),
        "workspace_file": attr.label(),
        "workspace_file_content": attr.string(),
        "build_file": attr.label(),
        "build_file_content": attr.string(),
    },
)
