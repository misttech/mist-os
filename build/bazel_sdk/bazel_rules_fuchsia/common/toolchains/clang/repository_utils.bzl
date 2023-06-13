# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Utilities related to Clang repositories. See README.md for details."""

load(":toolchains/clang/clang_utils.bzl", "process_clang_builtins_output")
load(":toolchains/clang/providers.bzl", "ClangInfo")

def prepare_clang_repository(repo_ctx, clang_install_dir):
    """Prepare a repository directory for a clang toolchain creation.

    This function must be called from a repository rule, and creates a number
    of files inside it.

    The files created by this function are:

      - Symlinks to the clang bin/, lib/ and other directories.

      - An `empty` file which is ... empty.

      - A `generated_constants.bzl` file that exports values corresponding
        to the Clang toolchain, and which can be loaded at load-time (e.g.
        from BUILD.bazel files).

      - A `debug_probe_output.txt` which is used to debug how the content of
        `generated_constants.bzl` was computed. Only used for debugging.

    This expects the caller to add a BUILD.bazel file to the repository that
    will call setup_clang_repository(), passing the value written into
    generated_constants.bzl to it.

    After this, functions from toolchain_utils.bzl can be used to create
    individual Clang Bazel toolchain instances.

    Args:
      repo_ctx: A repository_ctx value.
      clang_install_dir: Path to the clang prebuilt toolchain installation,
          relative to the main project workspace.
    """
    workspace_dir = str(repo_ctx.workspace_root)

    # Symlink the content of the clang installation directory into
    # the repository directory.

    # Symlink top-level items from Clang prebuilt install to repository directory
    # Note that this is possible because our C++ toolchain configuration redefine
    # the "dependency_file" feature to use relative file paths.
    clang_install_path = repo_ctx.path(workspace_dir + "/" + clang_install_dir)
    for f in clang_install_path.readdir():
        repo_ctx.symlink(f, f.basename)

    # Extract the builtin include paths by running the executable once.
    # Only care about C++ include paths. The list for compiling C is actually
    # smaller, but this is not an issue for now.
    #
    # Note that the paths must be absolute for Bazel, which matches the
    # output of the command, so there is not need to change them.
    #
    # Note however that Bazel will use relative paths when creating the list
    # of inputs for C++ compilation actions.

    # Create an empty file to be pre-processed. This is more portable than
    # trying to use /dev/null as the input.
    repo_ctx.file("empty", "")
    command = ["bin/clang", "-E", "-x", "c++", "-v", "./empty"]
    result = repo_ctx.execute(command)

    # Write the result to a file for debugging.
    repo_ctx.file("debug_probe_results.txt", result.stderr)

    short_version, long_version, builtin_include_paths = \
        process_clang_builtins_output(result.stderr)

    # Clang places a number of built-in headers (e.g. <mmintrin.h>) and
    # runtime libraries (e.g. libclang_rt.builtins.a) under a directory
    # whose location has changed over time.
    #
    # It used to be lib/clang/<long_version>/, but apparently this has
    # changed to lib/clang/<short_version>/ in clang-16. Support both schemes
    # by probing the file system.
    #
    lib_clang_internal_dir = None
    for version in [long_version, short_version]:
        candidate_dir = "lib/clang/" + version
        if repo_ctx.path(candidate_dir).exists:
            lib_clang_internal_dir = candidate_dir
            break

    if not lib_clang_internal_dir:
        fail("Could not find lib/clang/<version> directory!?")

    # Now convert that into a string that can go into a .bzl file.
    builtin_include_paths_str = "\n".join(["    \"%s\"," % path for path in builtin_include_paths])

    repo_ctx.file("generated_constants.bzl", content = '''
constants = struct(
  long_version = "{long_version}",
  short_version = "{short_version}",
  lib_clang_internal_dir = "{lib_clang_internal_dir}",
  builtin_include_paths = [
{builtin_paths}
  ],
)
'''.format(
        long_version = long_version,
        short_version = short_version,
        lib_clang_internal_dir = lib_clang_internal_dir,
        builtin_paths = builtin_include_paths_str,
    ))

def _clang_info_target_impl(ctx):
    return [ClangInfo(
        short_version = ctx.attr.short_version,
        long_version = ctx.attr.long_version,
        builtin_include_paths = ctx.attr.builtin_include_paths,
    )]

_clang_info_target = rule(
    implementation = _clang_info_target_impl,
    attrs = {
        "short_version": attr.string(
            doc = "Clang short version",
            mandatory = True,
        ),
        "long_version": attr.string(
            doc = "Clang long version",
            mandatory = True,
        ),
        "builtin_include_paths": attr.string_list(
            doc = "List of Clang builtin include paths, must be absolute.",
            mandatory = True,
            allow_empty = True,
        ),
    },
)

# buildifier: disable=unnamed-macro
def setup_clang_repository(constants):
    """Create a few required targets in a Clang repository.

    This function should be called early from the top-level BUILD.bazel
    in the Clang repository. It creates a few targets that functions in
    toolchain_utils.bzl rely on.

    Args:
      constants: The value written to generated_constants.bzl by
         a call to prepare_clang_repository() that was performed in the
         repository rule for the current Clang repository.
    """

    # Define the top-level `clang_info` target used to expose
    # the content of generated_constants.bzl to rule implementation functions.
    _clang_info_target(
        name = "clang_info",
        short_version = constants.short_version,
        long_version = constants.long_version,
        builtin_include_paths = constants.builtin_include_paths,
    )

    # The following filegroups are referenced from toolchain definitions
    # created by the generate_clang_cc_toolchain() function from
    # toolchain_utils.bzl.
    native.filegroup(
        name = "clang_empty",
        srcs = [],
    )

    native.filegroup(
        name = "clang_all",
        srcs = native.glob(
            ["**/*"],
            exclude = ["**/*.html", "**/*.pdf"],
        ),
    )

    native.filegroup(
        name = "clang_compiler_binaries",
        srcs = [
            "bin/clang",
            "bin/clang++",
            "bin/llvm-nm",
        ],
    )

    native.filegroup(
        name = "clang_linker_binaries",
        srcs = native.glob(["bin/*"]),  # TODO(digit): Restrict this
    )

    native.filegroup(
        name = "clang_ar_binaries",
        srcs = ["bin/llvm-ar"],
    )

    native.filegroup(
        name = "clang_objcopy_binaries",
        srcs = ["bin/llvm-objcopy"],
    )

    native.filegroup(
        name = "clang_strip_binaries",
        srcs = ["bin/llvm-strip"],
    )
