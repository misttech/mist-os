# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""All Fuchsia Providers."""

FuchsiaAssembledArtifactInfo = provider(
    "Artifacts that can be included into a product. It consists of the artifact and the corresponding config data.",
    fields = {
        "artifact": "The base artifact",
        "configs": "A list of configs that is attached to artifacts",
    },
)

FuchsiaConfigDataInfo = provider(
    "The config data which is used in assembly.",
    fields = {
        "source": "Config file on host",
        "destination": "A String indicating the path to find the file in the package on the target",
    },
)

FuchsiaComponentInfo = provider(
    "Contains information about a fuchsia component",
    fields = {
        "name": "name of the component",
        "manifest": "A file representing the compiled component manifest file",
        "resources": "any additional resources the component needs",
        "moniker": "The moniker to run the non-driver, non-test, non-session component in",
        "is_driver": "True if this is a driver",
        "is_test": "True if this is a test component",
        "run_tag": "A tag used to identify the component when put in a package to be later used by the run command",
    },
)

FuchsiaDeviceTreeSegmentInfo = provider(
    "Contains information about a fuchsia devicetree fragment",
    fields = {
        "includes": "A depset of include directory paths used when compiling the devicetree binary.",
        "files": "A depset of transitive dependencies needed for future devicetree compile.",
    },
)

FuchsiaPackagedComponentInfo = provider(
    "Contains information about a fuchsia component that has been included in a package",
    fields = {
        "component_info": "The original FuchsiaComponentInfo provider if this is built locally. Otherwise it will be empty",
        "dest": "The install location for this component in a package (meta/foo.cm)",
    },
)

def _fuchsia_unstripped_binary_info_init(*, unstripped_file, dest, stripped_file = None, source_dir = None):
    if not dest or type(dest) != "string":
        fail("Required 'dest' argument must be a string, got: %s" % repr(dest))
    if not unstripped_file or type(unstripped_file) != "File":
        fail("Required 'unstripped_file' argument must be a File, got: %s" % repr(unstripped_file))
    if stripped_file and type(stripped_file) != "File":
        fail("Optional 'stripped_file' argument must be a File, got: %s" % repr(stripped_file))
    if source_dir and type(source_dir) != "File":
        fail("Optional 'source_dir' argument must be a File, if not None, got: %s" % repr(source_dir))
    return {
        "dest": dest,
        "unstripped_file": unstripped_file,
        "stripped_file": stripped_file,
        "source_dir": source_dir,
        "never_forward": True,
    }

FuchsiaUnstrippedBinaryInfo, make_fuchsia_unstripped_binary_info = provider(
    "Contains information about one unstripped Fuchsia binary and its install location for the corresponding stripped file",
    fields = {
        "unstripped_file": "A required File value for the source unstripped ELF binary file.",
        "stripped_file": "Either None, or a File value for the corresponding stripped ELF binary file, if available as a prebuilt.",
        "dest": "A Fuchsia package install path string for the stripped file.",
        "source_dir": """Either None, or a File value pointing to a file or directory,
            see FuchsiaDebugSymbolInfo for documentation about this value. If None, the root workspace
            directory is used as the source directory.""",
        "never_forward": """A boolean whose value must be True. Its presence ensures that these values are
            never forwarded to dependents. See documentation for can_forward_provider() function.""",
    },
    init = _fuchsia_unstripped_binary_info_init,
)

FuchsiaCollectedUnstrippedBinariesInfo = provider(
    "Contains information about a set of unstripped ELF binaries.",
    fields = {
        "source_dir_to_unstripped_binary": """
            A { source_dir -> depset[struct(dest, unstripped_file, stripped_file)] } dictionary,
            Where 'unstripped_file' is a source File value for the unstripped file,
            where 'stripped_file' is either None, or a source File value for the corresponding
            stripped file if available as a prebuilt, and 'dest' is a install path string within
            a Fuchsia package for the corresponding stripped file.

            Where 'source_dir' is either a string or a File value describing the source
            directory used by the zxdb to locate sources at debug time. See FuchsiaDebugSymbolInfo
            for more details about this value.
            """,
    },
)

FuchsiaDebugSymbolInfo = provider(
    "Contains information that can be used to register debug symbols.",
    fields = {
        "build_id_dirs": """A { source_dir -> depset[build_dir] } dictionary, where 'build_dir'
            is a File value pointing to a .build-id/ directory, and 'source_dir' is either a
            string or a File value, used to locate source files when using the debugger.

            If 'source_dir' is a string, it is interpreted as an environment variable,
            which must be defined by Bazel when the action that registers debug symbols
            is run, such as BAZEL_WORKSPACE_DIRECTORY (see Bazel user manual).

            If 'source_dir' is a File pointing to a directory, the latter is used directly
            as a possible source directory.

            If 'source_dir' is a File pointing to a file, its parent directory is used
            instead as a possible source directory.
            """,
    },
)

FuchsiaComponentManifestInfo = provider(
    "Contains information about a Fuchsia component manifest",
    fields = {
        "compiled_manifest": "A File pointing to the compiled manifest",
        "component_name": "The name of the component",
        "config_package_path": "The path to the generated cvf file",
    },
)

FuchsiaComponentManifestShardInfo = provider(
    "Contains information about a Fuchsia component manifest shard",
    fields = {
        "file": "The file of the shard",
        "base_path": "Base path of the shard, used in includepath argument of cmc compile",
    },
)

FuchsiaComponentManifestShardCollectionInfo = provider(
    "Contains information about a collection of shards to add as dependencies for each cmc invocation",
    fields = {
        "shards": "A list of shards's as targets in the collection",
    },
)

FuchsiaFidlLibraryInfo = provider(
    "Contains information about a FIDL library",
    fields = {
        "info": "List of structs(name, files) representing the library's dependencies",
        "name": "Name of the FIDL library",
        "ir": "Path to the JSON file with the library's intermediate representation",
    },
)

FuchsiaBindLibraryInfo = provider(
    "Contains information about a Bind Library.",
    fields = {
        "name": "Name of the Bind Library.",
        "transitive_sources": "A depset containing transitive sources of the Bind Library.",
    },
)

FuchsiaCoreImageInfo = provider(
    "Private provider containing platform artifacts",
    fields = {
        "esp_blk": "EFI system partition image.",
        "kernel_zbi": "Zircon image.",
        "vbmetar": "vbmeta for zirconr boot image.",
        "zirconr": "zedboot boot image.",
    },
)

FuchsiaPackageResourcesInfo = provider(
    "Contains a collection of resources to include in a package",
    fields = {
        "resources": "A list of structs containing the src and dest of the resource",
    },
)

FuchsiaCollectedPackageResourcesInfo = provider(
    """A provider which represents a package resource and all of its transitive resources.

    This provider should not be directly created. If a rule wants to expose a set
    of resources it should create a FuchsiaPackageResourcesInfo provider instead.
    """,
    fields = {
        "collected_resources": "A depset containing the direct and transitive resources",
    },
)

FuchsiaPackageGroupInfo = provider(
    doc = "The raw files that make up a set of fuchsia packages.",
    fields = {
        "packages": "a list of all packages that make up this package group",
    },
)

FuchsiaPackageInfo = provider(
    doc = "Contains information about a fuchsia package.",
    fields = {
        "fuchsia_cpu": "The target CPU specified when building this package in fuchsia format (x64, arm64, riscv64)",
        "package_manifest": "JSON package manifest file representing the Fuchsia package.",
        "package_name": "The name of the package",
        "far_file": "The far archive",
        "meta_far": "The meta.far file",
        "files": "all files that compose this package, including the manifest and meta.far",
        "build_id_dirs": "Directories containing the debug symbols",
        "packaged_components": "A list of all the components in the form of FuchsiaPackagedComponentInfo structs",
        "package_resources": "A list of resources added to this package",
    },
)

FuchsiaProductImageInfo = provider(
    doc = "Info needed to pave a Fuchsia image",
    fields = {
        "esp_blk": "EFI system partition image.",
        "blob_blk": "BlobFS partition image.",
        "data_blk": "MinFS partition image.",
        "images_json": "images.json file",
        "blobs_json": "blobs.json file",
        "kernel_zbi": "Zircon image.",
        "vbmetaa": "vbmeta for zircona boot image.",
        "vbmetar": "vbmeta for zirconr boot image.",
        "zircona": "main boot image.",
        "zirconr": "zedboot boot image.",
        "flash_json": "flash.json file.",
    },
)

FuchsiaAssemblyConfigInfo = provider(
    doc = "Private provider that includes a single JSON configuration file.",
    fields = {
        "config": "JSON configuration file",
    },
)

FuchsiaProductBundleConfigInfo = provider(
    doc = "Config data used for pbm creation",
    fields = {
        "packages": "Path to packages directory.",
        "images_json": "Path to images.json file.",
        "zbi": "Path to ZBI file.",
        "fvm": "Path to FVM file.",
    },
)

FuchsiaProvidersInfo = provider(
    doc = """
    Keeps track of what providers exist on a given target.
    Construct with utils.bzl > track_providers.
    Used by utils.bzl > alias.
    """,
    fields = {
        "providers": "A list of providers values to carry forward.",
    },
)

FuchsiaVersionInfo = provider(
    doc = "version information passed in that overwrite sdk version",
    fields = {
        "version": "The version string.",
    },
)

AccessTokenInfo = provider(
    doc = "Access token used to upload to MOS repository",
    fields = {
        "token": "The token string.",
    },
)

FuchsiaPackageRepoInfo = provider(
    doc = "A provider which provides the contents of a fuchsia package repo",
    fields = {
        "packages": "The paths to the package_manifest.json files",
        "repo_dir": "The directory of the package repo.",
        "blobs": "The blobs needed by packages in this package repo.",
    },
)

FuchsiaRunnableInfo = provider(
    doc = "A provider which provides the script and runfiles to run a Fuchsia component or test package.",
    fields = {
        "executable": "A file corresponding to the runnable script.",
        "runfiles": "A list of runfiles that the runnable script depends on.",
        "is_test": "Whether this runnable is a test.",
    },
)

FuchsiaDriverToolInfo = provider(
    doc = "A provider which contains information about a driver tool.",
    fields = {
        "tool_path": "A tool's binary package-relative path (e.g. 'bin/tool').",
    },
)

FuchsiaProductBundleInfo = provider(
    doc = "Product Bundle Info.",
    fields = {
        "product_bundle": "The full URL for the product bundle. Can be empty.",
        "is_remote": "Whether the product bundle is a local path or a remote url.",
        "product_name": "The name of the product to be used if product_bundle is empty.",
        "product_version": "The version of the product to use.",
        "repository": "The name of the repository to host extra packages in the product bundle.",
        "build_id_dirs": "Directories containing the debug symbols",
    },
)

FuchsiaStructuredConfigInfo = provider(
    doc = "A provider which contains the generated cvf for structured configs.",
    fields = {
        "cvf_source": "The generated cvf",
        "cvf_dest": "The location where the cvf is stored within a fuchsia package archive.",
    },
)
