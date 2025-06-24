# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules used to define IDK atoms."""

load("@bazel_skylib//lib:paths.bzl", "paths")
load("@rules_cc//cc:defs.bzl", "cc_library")

_TYPES_SUPPORTING_UNSTABLE_ATOMS = [
    # LINT.IfChange(unstable_atom_types)
    "cc_source_library",
    "fidl_library",

    # LINT.ThenChange(//build/sdk/sdk_atom.gni:unstable_atom_types, //build/sdk/generate_idk/__init__.py:unstable_atom_types, //build/sdk/generate_prebuild_idk/idk_generator.py)
]
_TYPES_NOT_REQUIRING_COMPATIBILITY = [
    # LINT.IfChange(non_compatibility_types)
    "bind_library",
    "companion_host_tool",
    "dart_library",
    "data",
    "documentation",
    "experimental_python_e2e_test",
    "ffx_tool",
    "host_tool",
    "package",
    "version_history",
    # LINT.ThenChange(//build/sdk/sdk_atom.gni:non_compatibility_types)
]

# TOOD(https://fxbug.dev/417304469): `sdk_area`,  and some other
# fields of this provider do not belong in prebuild info. `idk_deps` may
# also be unnecessary, but could be useful for category enforcement.
FuchsiaIdkAtomInfo = provider(
    doc = "Defines an IDK atom",
    fields = {
        "label": "[label] The atom's label",
        "idk_name": "[string] Name of this atom within the IDK",
        "id": "[string] Identifier of this atom within the IDK",
        "meta_dest": "[string] Location of the atom's metadata file in the final IDK",
        "type": "[string] The type of atom",
        "category": "[string] The IDK category for the atom",
        "is_stable": "[bool] Whether the atom is stable",
        "api_area": "[string] The API area responsible for maintaining this atom",
        "api_file_path": "Path to the file representing the API canonically exposed by this atom.",
        "api_contents_map": "List of scopes for the files making up the atom's API.",
        "atom_files_map": "[dict[str,File]] a { dest -> source } map of files for this atom",
        "idk_deps": "[list[label]] Other atoms the atom directly depends on",
        "atoms_depset": "[depset[FuchsiaIdkAtomInfo]] The full set of other atoms the atom depends on",
        "atom_build_deps": "[list[label]] List of dependencies related to building the atom that should not be reflected in IDKs",
        "additional_prebuild_info": "[dict[str,list[Any]]] A dictionary of type-specific prebuild info for the atom. All values are lists, even if there is only one value",
    },
)

FuchsiaIdkMoleculeInfo = provider(
    doc = "Defines an IDK molecule, or group of atoms",
    fields = {
        "label": "The molecule's label",
        "idk_deps": "Atoms and other molecules the molecule depends on.",
        "atoms_depset": "depset[FuchsiaIdkAtomInfo] The full set atoms that make up the molecule",
    },
)

def _get_idk_label(label):
    return label + "_idk"

def _get_idk_deps(underlying_deps):
    idk_deps = []
    return [_get_idk_label(dep) for dep in underlying_deps] + idk_deps

def _create_idk_atom_impl(ctx):
    idk_deps = ctx.attr.idk_deps

    return [FuchsiaIdkAtomInfo(
        label = ctx.label,
        idk_name = ctx.attr.idk_name,
        id = ctx.attr.id,
        meta_dest = ctx.attr.meta_dest,
        type = ctx.attr.type,
        category = ctx.attr.category,
        is_stable = ctx.attr.stable,
        api_area = ctx.attr.api_area,
        api_file_path = ctx.attr.api_file_path,
        api_contents_map = ctx.attr.api_contents_map,
        atom_files_map = ctx.attr.files_map,
        idk_deps = idk_deps,
        atoms_depset = depset(
            direct = idk_deps,
            transitive = [dep[FuchsiaIdkAtomInfo].atoms_depset for dep in idk_deps],
        ),
        atom_build_deps = ctx.attr.atom_build_deps,
        additional_prebuild_info = ctx.attr.additional_prebuild_info,
    )]

_create_idk_atom = rule(
    doc = """Define an IDK atom. Do not instantiate directly - use `idk_atom()` instead.""",
    implementation = _create_idk_atom_impl,
    provides = [FuchsiaIdkAtomInfo],
    attrs = {
        "idk_name": attr.string(
            doc = "Name of this atom within the IDK.",
            mandatory = True,
        ),
        "id": attr.string(
            doc = "Identifier of this atom within the IDK. " +
                  "The identifier should represent the canonical base path of the element within " +
                  "the IDK according to the standard layout (https://fuchsia.dev/fuchsia-src/development/idk/layout.md)." +
                  "For an element at $ROOT/pkg/foo, the id should be `sdk://pkg/foo`.",
            mandatory = True,
        ),
        "meta_dest": attr.string(
            doc = "The path of the metadata file (usually `meta.json`) in the final IDK, relative to the IDK root.",
            mandatory = True,
        ),
        "type": attr.string(
            doc = "Type of the atom. Used to determine schema for this file. " +
                  "Metadata files are hosted under `//build/sdk/meta`. " +
                  'If the metadata conforms to `//build/sdk/meta/foo.json`, the present attribute should have a value of "foo".',
            mandatory = True,
        ),
        "category": attr.string(
            doc = """Describes the availability of the element.
Possible values, from most restrictive to least restrictive:
    - compat_test : May be used to configure and run CTF tests but may not be exposed for use
                    in production in the IDK or used by host tools.
    - host_tool   : May be used by host tools (e.g., ffx) provided by the platform organization
                    but may not be used by production code or prebuilt binaries in the IDK.
    - prebuilt    : May be part of the ABI that prebuilt binaries included in the IDK use to
                    interact with the platform.
    - partner     : Included in the IDK for direct use of the API by out-of-tree developers.""",
            mandatory = True,
        ),
        "stable": attr.bool(
            doc = "Whether this atom is stabilized. " +
                  'Must be specified for types "fidl_library" and "cc_source_library" and otherwise unspecified. ' +
                  "This is only informative. The value must match the `stable` value in the atom metadata specified by `source`/`value`. " +
                  "(That metadata is what controls whether the atom is marked as unstable in the final IDK.)",
            mandatory = True,
        ),
        "api_area": attr.string(
            doc = "The API area responsible for maintaining this atom. " +
                  "See docs/contribute/governance/areas/_areas.yaml for the list of areas. " +
                  '"Unknown" is also a valid option. By default, the area will be `null` in the build manifest.',
            mandatory = False,
            default = "",
        ),
        "api_file_path": attr.string(
            doc = "Path to the file representing the API canonically exposed by this atom. " +
                  "This file is used to ensure modifications to the API are explicitly acknowledged. " +
                  "If this attribute is set, `api_contents_map` must be set as well. If not specified, no such modification checks are performed.",
            mandatory = False,
            default = "",
        ),
        "api_contents_map": attr.string_keyed_label_dict(
            doc = "A dictionary of files making up the atom's API, mapping the destination path " +
                  "of  a file relative to the IDK root to its source file label. " +
                  "The set of files will be used to verify that the API has not changed locally. " +
                  "This is very roughly approximated by checking whether the files themselves have changed at all." +
                  "Required when when `api_file_path` is set.",
            mandatory = False,
            default = {},
            allow_files = True,
        ),
        "files_map": attr.string_keyed_label_dict(
            doc = "A dictionary of files for this atom, mapping the destination " +
                  "path of a file relative to the IDK root to its source file label.",
            mandatory = False,
            default = {},
            allow_files = True,
        ),
        "idk_deps": attr.label_list(
            providers = [FuchsiaIdkAtomInfo],
            doc = "Bazel labels for other SDK elements this element publicly depends on at build time." +
                  "These labels must point to `_create_idk_atom` targets.",
            mandatory = False,
        ),
        "atom_build_deps": attr.label_list(
            providers = [DefaultInfo],
            doc = "List of dependencies related to building the atom that should not be reflected in IDKs. " +
                  "Mostly useful for code generation and validation.",
            mandatory = False,
        ),
        "additional_prebuild_info": attr.string_list_dict(
            doc = "A dictionary of type-specific prebuild info for the atom. " +
                  "All values are lists, even if there is only one value.",
            mandatory = False,
            default = {},
        ),
    },
)

# TODO(https://fxbug.dev/417305295): Make this a symbolic macro after updating
# to Bazel 8 and move all the args descriptions from _create_idk_atom() to here
# and reference those definitions from _create_idk_atom().
def idk_atom(
        name,
        type,
        stable,
        api_file_path = None,
        api_contents_map = None,
        **kwargs):
    """Generate an IDK atom and ensure proper validation of it.

    Args:
        name: The name of the IDK atom target.
        type: See _create_idk_atom().
        stable:  See _create_idk_atom().
        api_file_path: See _create_idk_atom().
        api_contents_map:  See _create_idk_atom().
        **kwargs: Additional arguments for the underlying atom.  See _create_idk_atom().
    """

    if type not in _TYPES_SUPPORTING_UNSTABLE_ATOMS and not stable:
        fail("`stable` must be true unless the type ('%s') is one of %s." % (type, _TYPES_SUPPORTING_UNSTABLE_ATOMS))

    if (not api_file_path) != (not api_contents_map):
        fail("`api_file_path` and `api_contents_map` must be specified together.")

    is_type_not_requiring_compatibility = type in _TYPES_NOT_REQUIRING_COMPATIBILITY
    if stable and not api_file_path and not is_type_not_requiring_compatibility:
        fail("All atoms with types ('%s') requiring compatibility must specify an `api_file_path` unless explicitly unstable." % type)

    # TODO(https://fxbug.dev/417305295): Verify the API with
    # `api_file_path` and `api_contents_map`.

    # TODO(https://fxbug.dev/417305295): Generate internal metadata (.sdk) file
    # if necessary. See https://fxbug.dev/407083737.

    # TODO(https://fxbug.dev/417305295): Verify category compatibility,
    # `api_area`, etc. Some of this could potentially be deferred until
    # generate_idk, especially if not building the internal build metadata (see
    # above).

    _create_idk_atom(
        name = name + "_idk",
        type = type,
        stable = stable,
        api_file_path = api_file_path,
        api_contents_map = api_contents_map,
        **kwargs
    )

def _idk_molecule_impl(ctx):
    idk_deps = ctx.attr.deps

    # Build the atoms depset, excluding molecules while including their atoms.
    direct_deps = []
    transitive_depsets = []
    for dep in idk_deps:
        if FuchsiaIdkAtomInfo in dep:
            direct_deps.append(dep)
            transitive_depsets.append(dep[FuchsiaIdkAtomInfo].atoms_depset)
        elif FuchsiaIdkMoleculeInfo in dep:
            transitive_depsets.append(dep[FuchsiaIdkMoleculeInfo].atoms_depset)
        else:
            fail("Unexpected dependency %s. Must be an atom or a molecule." % dep)

    atoms_depset = depset(direct = direct_deps, transitive = transitive_depsets)

    return [FuchsiaIdkMoleculeInfo(
        label = ctx.label,
        idk_deps = ctx.attr.deps,
        atoms_depset = atoms_depset,
    )]

idk_molecule = rule(
    doc = "Generate an IDK molecule.",
    implementation = _idk_molecule_impl,
    attrs = {
        "deps": attr.label_list(
            providers = [[FuchsiaIdkAtomInfo], [FuchsiaIdkMoleculeInfo]],
            mandatory = True,
            doc = "Atoms and other molecules the molecule depends on.",
        ),
    },
)

# TODO(https://fxbug.dev/417305295): Make this a symbolic macro after updating
# to Bazel 8.
# TODO(https://fxbug.dev/417305295): Consider the best order for arguments and
# whether they should follow the order of arguments in the underlying rule
# (https://bazel.build/reference/be/c-cpp#cc_library) or the results of
# `fx format-code`, which reorders most non-common arguments alphabetically at
# call sites.
# TODO(https://fxbug.dev/417305295): Add the following argument supported by
# the GN `sdk_source_set()` template if necessary. Note that
# `sdk_static_library()` and `sdk_shared_library()` do not support it.
# When adding support, assert that it is only used for
# "//sdk/fidl/zbi:zbi.c.checked-in"
# * `non_idk_implementation_deps` (GN equivalent: `non_sdk_deps`)
def idk_cc_source_library(
        name,
        idk_name,
        category,
        stable,
        api_area = None,
        api_file_path = None,
        deps = [],
        # TODO(https://fxbug.dev/417307356): When implementing prebuilt
        # libraries, add `data = []`, which will be equivalent to GN's
        # `runtime_deps`.
        srcs = [],
        hdrs = [],
        implementation_deps = [],
        include_base = "include",
        # TODO(https://fxbug.dev/417306131): Implement PlaSA support.
        # idk_headers_for_internal_use = [],
        testonly = False,
        visibility = ["//visibility:private"],
        # TODO(https://fxbug.dev/417305295): Add this argument if there is a way
        # to tell Bazel to not always compile the source set.
        # build_as_static = False,
        **kwargs):
    """Defines a C++_source library that can be exported to an IDK.

    Args:
        name: The name of the underlying `cc_library` target.
            GN equivalent: `target_name`
        idk_name: Name of the library in the IDK. Usually matches `name`.
            GN equivalent: `sdk_name`
        category: Publication level of the library in the IDK. See _create_idk_atom().
        stable: Whether this source library is stabilized.
            When true, an .api file is generated. When false, the atom is marked
            as unstable in the final IDK.
            Must be specified when `category` is defined
        api_area: The API area responsible for maintaining this library.
            GN equivalent: `sdk_area`
        api_file_path: Override path for the file representing the API of this library.
            This file is used to ensure modifications to the library's API are
            explicitly acknowledged.
            If not specified, the path will be "<idk_name>.api".
            GN equivalent: ``api`
            Not allowed when `stable` is false.
        deps: List of labels for other IDK elements this element publicly depends on at build time.
            These labels must point to targets with corresponding `_create_idk_atom` targets.
            GN equivalent: `public_deps`
        srcs: The list of C and C++ source and header files that are processed to create the library target.
            GN equivalent: `sources`
            Unlike the GN template, public headers must actually be in `hdrs`.
        hdrs: The list of C and C++ header files published by this library to be directly included by sources in dependent rules.
            GN equivalent: `public`
        implementation_deps: List of labels for other IDK elements this element depends on at build time.
            These labels must point to targets with corresponding `_create_idk_atom` targets.
            GN equivalent: `deps`.
        include_base: Path to the root directory for includes.
            This path will be added to the underlying libraries `includes`.
            GN note: This is a change in behavior from the GN template.
        # TODO(https://fxbug.dev/417306131): Implement PlaSA support.
        # idk_headers_for_internal_use:
            GN equivalent: `sdk_headers_for_internal_use`
        testonly: Standard definition.
        visibility: Standard definition.
        **kwargs: Additional arguments for the underlying library.
    """

    if category not in ["partner"]:
        # Other categories are only to ensure ABI compatibility and thus not
        # applicable.
        fail("Category '%s' is not supported." % category)
    if api_file_path and not stable:
        fail("Unstable targets do not require/support modification acknowledgement.")

    if category != "partner":
        fail("Create a separate allowlist when adding support for other categories.")
    if stable:
        allowlist_deps = ["//build/bazel/bazel_idk:partner_idk_source_sets_allowlist"]
    else:
        allowlist_deps = ["//build/bazel/bazel_idk:partner_idk_unstable_source_sets_allowlist"]

    # TODO(https://fxbug.dev/421888626): Apply the equivalent of GN's
    # `default_common_binary_configs` using the `copts` argument.
    # TODO(https://fxbug.dev/421888626): Add "//build/config:sdk_extra_warnings"
    # using the `copts` argument.
    cc_library(
        name = name,
        srcs = srcs,
        data = allowlist_deps,
        hdrs = hdrs,
        deps = deps,
        # TODO(https://fxbug.dev/417305295): If we must support
        # `non_idk_implementation_deps`, add it below.
        implementation_deps = implementation_deps,
        includes = [include_base],
        linkstatic = True,
        testonly = testonly,
        visibility = visibility,
        **kwargs
    )

    idk_root_path = "pkg/" + idk_name
    include_dest = idk_root_path + "/include"

    # Determine destinations in the IDK for headers and sources.
    idk_metadata_headers = []
    idk_metadata_sources = []
    idk_header_files_map = {}

    for header in hdrs:
        relative_destination = paths.relativize(header, include_base)
        destination = include_dest + "/" + relative_destination
        idk_metadata_headers.append(destination)
        idk_header_files_map |= {destination: header}

    idk_files_map = dict(idk_header_files_map)

    for source in srcs:
        source_dest_path = idk_root_path + "/" + source
        idk_metadata_sources.append(source_dest_path)
        idk_files_map |= {source_dest_path: source}

    # TODO(https://fxbug.dev/417305295): Verify pragma. Add to `atom_build_deps`.
    # TODO(https://fxbug.dev/417305295): Verify public headers. Add to
    # `atom_build_deps`.
    # TODO(https://fxbug.dev/417306131): Implement PlaSA support. Add to
    # `atom_build_deps`.

    idk_deps = _get_idk_deps(deps) + _get_idk_deps(implementation_deps)

    # Dependencies for generating the actual IDK atom (not the underlying library).
    # TODO(https://fxbug.dev/417305295): If we must support
    # `non_idk_implementation_deps`, add it here.
    # TODO(https://fxbug.dev/417307356): When implementing prebuilt
    # libraries, add `[":%s" % name]`. It may also be necessary to return
    # `DefaultInfo` from the underlying library.
    atom_build_deps = []

    if stable:
        api_path = idk_name + ".api"
        if api_file_path:
            if paths.relativize(api_file_path, ".") == paths.relativize(api_path, "."):
                fail("The specified `api` file (`%s`) matches the default. `api` only needs to be specified when overriding the default." % api_file_path)
            api_path = api_file_path

        api_contents_map = idk_header_files_map
    else:
        api_path = None
        api_contents_map = None

    # The atom's visibility should allow IDK contents/definition rules to depend
    # on the atom in addition to the visibility of the underlying library.
    # The built-in visibility labels cannot be used in combination with other
    # labels so handle them specificcally.
    # Note: This does not support visibility between IDK atoms in the same
    # package when visibility is "//visibility:private".
    if "//visibility:public" in visibility:
        atom_visibility = visibility
    else:
        # TODO(https://fxbug.dev/42073212): Add the path to where the IDK
        # contents are defined.
        atom_visibility = []
        if "//visibility:private" not in visibility:
            atom_visibility += visibility

    idk_atom(
        name = name,
        idk_name = idk_name,
        id = "sdk://" + idk_root_path,
        meta_dest = idk_root_path + "/meta.json",
        # Note: meta.value/source in the GN template are not needed because the
        # metadata is generated from the prebuild data.
        type = "cc_source_library",
        category = category,
        stable = stable,
        api_area = api_area,
        api_file_path = api_path,
        api_contents_map = api_contents_map,
        files_map = idk_files_map,
        idk_deps = idk_deps,
        atom_build_deps = atom_build_deps,
        additional_prebuild_info = {
            "include_dir": [include_dest],
            "sources": idk_metadata_sources,
            "headers": idk_metadata_headers,
            # TODO(https://fxbug.dev/417304469): Standardize on using a common
            # `idk_deps` for all atoms.
            "deps": idk_deps,
            # TODO(https://fxbug.dev/417304469): Standardize on using `idk_name`
            # for all atoms.
            "library_name": [idk_name],
            "file_base": [idk_root_path],
        },
        testonly = testonly,
        visibility = atom_visibility,
    )

    # TODO(https://fxbug.dev/417305295):Implement the //sdk:sdk_source_set_list
    # build API module and merge with the GN data.
    # sdk_source_set_sources = srcs + hdrs
