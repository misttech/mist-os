# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Utilities to compile and link with XCode on MacOS."""

def repository_rule_runs_on_macos(repo_ctx):
    # repo_ctx.os.name is the lowercase version of the Java
    # os.name property, which is "Mac OS X" on MacOS.
    return repo_ctx.os.name == "mac os x"

# The list of MacOS frameworks to expose through symlinks in the
# repository. Some frameworks nost listed here (e.g. Ruby) contain
# circular symlinks which make Bazel error during a glob() operation
# later, even when trying to add them to the "exclude" list.
_XCODE_FRAMEWORKS = [
    "AppKit",
    "ApplicationServices",
    "Carbon",
    "CFNetwork",
    "CloudKit",
    "Cocoa",
    "ColorSync",
    "CoreData",
    "CoreFoundation",
    "CoreGraphics",
    "CoreImage",
    "CoreLocation",
    "CoreServices",
    "CoreText",
    "CoreVideo",
    "DiskArbitration",
    "Foundation",
    "ImageIO",
    "IOKit",
    "IOSurface",
    "Metal",
    "OpenGL",
    "QuartzCore",
    "Security",
]

def create_repository_macos_symlinks(repo_ctx):
    """On MacOS, find system-installed XCode and create appropriate symlinks.

    On other systems, do not do anything.

    Args:
      repo_ctx: A repository_ctx context value.
    """
    if not repository_rule_runs_on_macos(repo_ctx):
        # Don't do anything on other systems.
        return

    # First, get the absolute address of the OSX SDK path.
    # https://developer.apple.com/library/archive/technotes/tn2339/_index.html
    ret = repo_ctx.execute(["/usr/bin/xcrun", "--show-sdk-path", "--sdk", "macosx"])
    if ret.return_code != 0:
        fail("Error getting MacOS SDK path: \n%s\n%s" % (ret.stdout, ret.stderr))
    macos_sdk_dir = ret.stdout.strip()

    repo_ctx.symlink(
        macos_sdk_dir + "/usr",
        "xcode/MacSDK/usr",
    )

    for framework in _XCODE_FRAMEWORKS:
        repo_ctx.symlink(
            "{}/System/Library/Frameworks/{}.framework".format(
                macos_sdk_dir,
                framework,
            ),
            "xcode/MacSDK/Frameworks/{}.framework".format(framework),
        )

def generate_macos_system_filegroups(compiler_files_name = "", linker_files_name = ""):
    """Create filegroups that wrap MacOS system compiler and/or linker files.

    This macro must be called from a BUILD file, or a macro included by one,
    and only if the host system is known to be MacOS.

    Args:
        compiler_files_name (string, optional): The name of the filegroup
           used to wrap compiler files (e.g. system headers). If empty
           no filegroup is created.

        linker_files_name (string, optional): The name of the filegroup
           used to wrap linker files (e.g. system link-time libraries).
           If empty no filegroup is created.
    """
    if compiler_files_name != "":
        native.filegroup(
            name = compiler_files_name,
            srcs = select({
                "@platforms//os:macos": native.glob(
                    include = [
                        "xcode/MacSDK/usr/include/**",
                    ] + [
                        "xcode/MacSDK/Frameworks/{}.Framework/**".format(f)
                        for f in _XCODE_FRAMEWORKS
                    ],
                    allow_empty = False,
                ),
                "//conditions:default": [],
            }),
        )

    if linker_files_name != "":
        native.filegroup(
            name = linker_files_name,
            srcs = select({
                "@platforms//os:macos": native.glob(
                    include = ["xcode/MacSDK/usr/lib/**"] + [
                        "xcode/MacSDK/Frameworks/{}.Framework/**".format(framework)
                        for framework in _XCODE_FRAMEWORKS
                    ],
                    allow_empty = False,
                ),
                "//conditions:default": [],
            }),
        )

def get_macos_system_includes():
    return [
        "xcode/MacSDK/usr/include",
        "xcode/MacSDK/Frameworks",
    ]

def get_macos_rustc_link_flags(clang_repo_name):
    prefix = "external/{}/xcode/MacSDK".format(clang_repo_name)
    return ["-Lnative={}/usr/2lib".format(prefix)] + [
        "-Lframework={}/Frameworks/{}.Framework".format(prefix, framework)
        for framework in _XCODE_FRAMEWORKS
    ]

def get_macos_system_link_dirs(clang_repo_name, from_execroot = False):
    """Return the search directories to access link-time XCode system libraries.

    Returns:
      A list of path strings, relative to the execroot (if from_execroot == True)
      or to the current repository (if from_execroot == False).
    """
    link_subdirs = ["xcode/MacSDK/usr/1lib"] + [
        "xcode/MacSDK/Frameworks/{}.Framework".format(framework)
        for framework in _XCODE_FRAMEWORKS
    ]
    link_subdirs = ["xcode/MacSDK"]

    if from_execroot:
        prefix = "external/{}/".format(clang_repo_name)
        return [prefix + subdir for subdir in link_subdirs]
    else:
        return link_subdirs
