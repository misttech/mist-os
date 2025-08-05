# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rules used to define IDK atoms."""

load("@bazel_skylib//lib:paths.bzl", "paths")
load("@fuchsia_build_info//:args.bzl", "warn_on_sdk_changes")
load("@rules_cc//cc:defs.bzl", "cc_library")
load(":cpp_verification.bzl", "verify_no_pragma_once")
load("//build/bazel/rules:golden_files.bzl", "verify_golden_files")

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

def _get_idk_label(label_str):
    # Ensure the label is relative to the `BUILD` file, not this `.bzl` file
    # in cases where `label_str` omits the package (e.g., ":target_name").
    label = native.package_relative_label(label_str)

    # Build the label to handle cases where `label_str` omits the target name
    # (e.g., "//path/to/package").
    return "//{}:{}_idk".format(label.package, label.name)

def _get_idk_deps(underlying_deps):
    idk_deps = []
    return [_get_idk_label(dep) for dep in underlying_deps] + idk_deps

def _compute_atom_api_impl(ctx):
    args = ctx.actions.args()
    args.add("--output", ctx.outputs.generated_api_file.path)

    for dest_path, source_target in ctx.attr.api_contents_map.items():
        source_label = source_target.label
        source_path = source_label.package + "/" + source_label.name

        # `add()` supports at most two parameters, so add the third separately.
        args.add("--file", dest_path)
        args.add(source_path)

    # `ctx.files.api_contents_map` contains just the source files.
    inputs_depset = depset(ctx.files.api_contents_map)

    ctx.actions.run(
        outputs = [ctx.outputs.generated_api_file],
        inputs = inputs_depset,
        executable = ctx.executable._script,
        arguments = [args],
        mnemonic = "ComputeAtomApi",
        progress_message = "Computing API for %s" % ctx.outputs.generated_api_file.short_path,
    )

    return [DefaultInfo(files = depset([ctx.outputs.generated_api_file]))]

_compute_atom_api = rule(
    doc = "Computes the contents of the .api file for an atom.",
    implementation = _compute_atom_api_impl,
    provides = [DefaultInfo],
    attrs = {
        "api_contents_map": attr.string_keyed_label_dict(
            doc = "A dictionary of files that make up the API for this atom, " +
                  "mapping the destination path of a file relative to the " +
                  "IDK  root to its source file label.",
            mandatory = True,
            allow_empty = False,
            default = {},
            allow_files = True,
        ),
        "generated_api_file": attr.output(
            mandatory = True,
            doc = "The output API file.",
        ),
        "_script": attr.label(
            default = Label("//build/sdk:compute_atom_api"),
            executable = True,
            cfg = "exec",
        ),
    },
)

def _create_idk_atom_impl(ctx):
    all_deps_depset = depset(direct = ctx.files.idk_deps + ctx.files.atom_build_deps)
    idk_deps = ctx.attr.idk_deps

    if (not ctx.attr.api_file_path) != (not ctx.attr.api_contents_map):
        fail("`api_file_path` and `api_contents_map` must be specified together.")

    return [
        DefaultInfo(files = all_deps_depset),
        FuchsiaIdkAtomInfo(
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
        ),
    ]

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
                  '"Unknown" is also a valid option.',
            mandatory = True,
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
                  "Required and must not be empty when when `api_file_path` is set.",
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
# to Bazel 8. Consider moving all the args descriptions from _create_idk_atom()
# to here and reference those definitions from _create_idk_atom(). Replace
# "Required" comments with `mandatory = True`.
def idk_atom(
        name,
        type,
        stable,
        testonly,
        api_file_path = None,
        api_contents_map = None,
        atom_build_deps = [],
        **kwargs):
    """Generate an IDK atom and ensure proper validation of it.

    Args:
        name: The name of the IDK atom target. Required.
        type: See _create_idk_atom(). Required.
        stable:  See _create_idk_atom(). Required.
        testonly: Standard definition. Required.
        api_file_path: See _create_idk_atom().
        api_contents_map:  See _create_idk_atom().
        atom_build_deps:  See _create_idk_atom().
        **kwargs: Additional arguments for the underlying atom.  See _create_idk_atom().
    """

    if type not in _TYPES_SUPPORTING_UNSTABLE_ATOMS and not stable:
        fail("`stable` must be true unless the type ('%s') is one of %s." % (type, _TYPES_SUPPORTING_UNSTABLE_ATOMS))

    if (not api_file_path) != (not api_contents_map):
        fail("`api_file_path` and `api_contents_map` must be specified together.")

    is_type_not_requiring_compatibility = type in _TYPES_NOT_REQUIRING_COMPATIBILITY
    if stable and not api_file_path and not is_type_not_requiring_compatibility:
        fail("All atoms with types ('%s') requiring compatibility must specify an `api_file_path` unless explicitly unstable." % type)

    _verify_api = api_file_path != None
    if _verify_api:
        if not api_contents_map:
            fail("`api_contents_map` cannot be empty.")

        generate_api_target_name = "%s_generate_api" % name
        verify_api_target_name = "%s_verify_api" % name

        # GN-generated files generally have `_sdk` from the target name.
        # TODO(https://fxbug.dev/425931839): Change this to `_idk` or drop it
        # once GN is no longer generating such files.
        current_api_file = "%s_sdk.api" % name

        _compute_atom_api(
            name = generate_api_target_name,
            api_contents_map = api_contents_map,
            generated_api_file = current_api_file,
            testonly = testonly,
            visibility = ["//visibility:private"],
        )

        verify_golden_files(
            name = verify_api_target_name,
            candidate_files = [current_api_file],
            golden_files = [api_file_path],
            only_warn_on_changes = warn_on_sdk_changes,
            testonly = testonly,
            visibility = ["//visibility:private"],
        )

        atom_build_deps.append(":%s" % verify_api_target_name)

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
        atom_build_deps = atom_build_deps,
        testonly = testonly,
        **kwargs
    )

def _idk_molecule_impl(ctx):
    all_deps_depset = depset(direct = ctx.files.deps)
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

    return [
        DefaultInfo(files = all_deps_depset),
        FuchsiaIdkMoleculeInfo(
            label = ctx.label,
            idk_deps = ctx.attr.deps,
            atoms_depset = atoms_depset,
        ),
    ]

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
# to Bazel 8. Replace "Required" comments with `mandatory = True`.
# TODO(https://fxbug.dev/428229472): When migrating "zbi-format":
# * add `non_idk_implementation_deps` (GN equivalent: `non_sdk_deps`) argument.
# * assert that it is only used for "//sdk/fidl/zbi:zbi.c.checked-in".
# * Add TODO for https://fxbug.dev/42062786 to remove the argument when fixing the bug.
def idk_cc_source_library(
        name,
        idk_name,
        category,
        stable,
        api_area,
        hdrs,
        hdrs_for_internal_use = [],
        srcs = [],
        deps = [],
        implementation_deps = [],
        # TODO(https://fxbug.dev/417307356): When implementing prebuilt
        # libraries, add `data = []`, which will be equivalent to GN's
        # `runtime_deps`.
        include_base = "include",
        api_file_path = None,
        testonly = False,
        visibility = ["//visibility:private"],
        # TODO(https://fxbug.dev/425931839): Remove these no longer converting to GN.
        build_as_static = False,  # buildifier: disable=unused-variable - For GN conversion only.
        public_configs = [],  # buildifier: disable=unused-variable - For GN conversion only.
        **kwargs):
    """Defines a C++_source library that can be exported to an IDK.

    Args:
        name: The name of the underlying `cc_library` target. Required.
            GN equivalent: `target_name`
        idk_name: Name of the library in the IDK. Usually matches `name`. Required.
            GN equivalent: `sdk_name`
        category: Publication level of the library in the IDK. See _create_idk_atom(). Required.
        stable: Whether this source library is stabilized.
            When true, an .api file is generated. When false, the atom is marked
            as unstable in the final IDK. Required.
        api_area: The API area responsible for maintaining this library. Required.
            GN equivalent: `sdk_area`
        hdrs: The list of C and C++ header files published by this library to be directly included
            by sources in dependent rules. Does not include internal headers that are included from
            public headers but not meant to be included by dependents - see `hdrs_for_internal_use`.
            Atoms providing headers used by these headers must be included in the (public) `deps`.
            Required and may not be empty.
            GN equivalent: `public`
            GN note: Unlike the GN template, this list does not include `hdrs_for_internal_use`.
        hdrs_for_internal_use: List of C and C++ headers included by headers in `hdrs` that are not
            meant to be included by a client of this library. They usually contain implementation
            details. Their contents are not included in documentation but they are included in the
            `headers` metadata for the IDK library. They may be excluded from some but not all API
            compatibility checks.
            Like `hdrs`, the atoms providing headers used by these headers must be included in the
            (public) `deps`.
            GN equivalent: `sdk_headers_for_internal_use`
            GN note: Unlike the GN template where this argument specifices headers already included
            elsewhere, such headers are only listed here.
        srcs: The list of C and C++ source and header files that are processed to create the
            library target, excluding those in `hdrs` and hdrs_for_internal_use.
            Header files in this list can only be included by this library.
            GN equivalent: `sources`
            GN note: Unlike the GN template, public headers must actually be in `hdrs`.
        deps: List of labels for other IDK elements this element publicly depends on at build time.
            These labels must point to targets with corresponding `_create_idk_atom` targets.
            GN equivalent: `public_deps`
        implementation_deps: List of labels for other IDK elements this element depends on at build time.
            These labels must point to targets with corresponding `_create_idk_atom` targets.
            GN equivalent: `deps`.
        include_base: Path to the root directory for includes.
            This path will be added to the underlying library's `includes`.
            If the path is "//sdk", the paths to the headers will be made
            relative to `//sdk`. Otherwise, `include_base` will be removed from
            the header paths.
            GN note: This preserves the behavior of the GN template.
        api_file_path: Override path for the file representing the API of this library.
            This file is used to ensure modifications to the library's API are
            explicitly acknowledged.
            If not specified, the path will be "<idk_name>.api".
            GN equivalent: `api`
            Not allowed when `stable` is false.
        testonly: Standard definition.
        visibility: Standard definition.
        build_as_static: Unused in Bazel, for GN conversion only.
            TODO(https://fxbug.dev/417305295): Use this argument if there is a
             way to tell Bazel to not always compile the source set.
        public_configs: Unused in Bazel, for GN conversion only.
        **kwargs: Additional arguments for the underlying library.
    """

    # TODO(https://fxbug.dev/417305295): Replace this with `allow_empty = False`
    # when converting this macro to a symbolic macro.
    if not hdrs:
        fail("`hdrs` cannot be empty.")

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

    # Group the source files for various uses.
    # Per https://bazel.build/versions/6.2.0/reference/be/c-cpp#cc_library.hdrs,
    # "Headers not meant to be included by a client of this library should be
    # listed in the srcs attribute instead, even if they are included by a
    # published header." Thus, `hdrs_for_internal_use` is added to `srcs` in the
    # underlying library, not `hdrs`. However, other build systems that use the
    # IDK may not  work this way, so include `hdrs_for_internal_use` as headers
    # in the IDK. This is also consistent with prebuilt libraries where the IDK
    # only includes headers.
    all_source_files = hdrs + hdrs_for_internal_use + srcs
    hdrs_for_idk = hdrs + hdrs_for_internal_use
    hdrs_for_bazel_library = hdrs
    srcs_for_bazel_library = srcs + hdrs_for_internal_use

    if include_base == "//sdk":
        # Some libraries in //sdk/lib/<library_name>[/...] rely on that in-tree
        # path to allow in-tree code to include `<lib/library_name/header.h>`
        # and for the IDK destination path rather than providing that structure
        # within the `library_name` directory. Handle this case here.

        # Get the relative path from the build file's directory to `//sdk`,
        # which is the real include base for in-tree builds.
        path_to_this_directory = "//" + native.package_name()
        this_directory_relative_to_sdk = paths.relativize(path_to_this_directory, "//sdk")
        include_path = ""
        for _ in this_directory_relative_to_sdk.split("/"):
            include_path += "../"
    else:
        this_directory_relative_to_sdk = None  # Satisfy buildifier.
        include_path = include_base

    # TODO(https://fxbug.dev/421888626): Apply the equivalent of GN's
    # `default_common_binary_configs` using the `copts` argument.
    # TODO(https://fxbug.dev/421888626): Add "//build/config:sdk_extra_warnings"
    # using the `copts` argument.
    cc_library(
        name = name,
        srcs = srcs_for_bazel_library,
        data = allowlist_deps,
        hdrs = hdrs_for_bazel_library,
        deps = deps,
        # TODO(https://fxbug.dev/428229472): If we must support
        # `non_idk_implementation_deps`, include it below.
        implementation_deps = implementation_deps,
        includes = [include_path],
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

    for header in hdrs_for_idk:
        if include_base == "//sdk":
            # As above, handle the special case.
            relative_destination = paths.join(this_directory_relative_to_sdk, header)
        else:
            relative_destination = paths.relativize(header, include_base)

        destination = include_dest + "/" + relative_destination
        idk_metadata_headers.append(destination)
        idk_header_files_map |= {destination: header}

    idk_files_map = dict(idk_header_files_map)

    for source in srcs:
        source_dest_path = idk_root_path + "/" + source
        idk_metadata_sources.append(source_dest_path)
        idk_files_map |= {source_dest_path: source}

    idk_deps = _get_idk_deps(deps) + _get_idk_deps(implementation_deps)

    # Dependencies for generating the actual IDK atom (not the underlying library).
    # TODO(https://fxbug.dev/428229472): If we must support
    # `non_idk_implementation_deps`, add it here.
    # TODO(https://fxbug.dev/417307356): When implementing prebuilt
    # libraries, add `[":%s" % name]`. It may also be necessary to return
    # `DefaultInfo` from the underlying library.
    atom_build_deps = []

    # Ensure there are no duplicate files specified by providing all source
    # files as a single list of labels. Bazel will report an error if the
    # combined list containes duplicates. All this target really does is provide
    # a clearer error message than if we relied on combining the lists in the
    # `verify_no_pragma_once()` rule below.
    verify_no_duplicate_files_target_name = "%s_verify_no_duplicate_files_in_hdrs_and_srcs" % name
    native.filegroup(
        name = verify_no_duplicate_files_target_name,
        data = all_source_files,
        testonly = testonly,
        visibility = ["//visibility:private"],
    )
    atom_build_deps.append(":%s" % verify_no_duplicate_files_target_name)

    # For simplicity, check all source files, including non-header files in `srcs`.
    verify_pragma_once_target_name = "%s_verify_pragma_once" % name
    verify_no_pragma_once(
        name = verify_pragma_once_target_name,
        files = all_source_files,
        testonly = testonly,
        visibility = ["//visibility:private"],
    )
    atom_build_deps.append(":%s" % verify_pragma_once_target_name)

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
    # sdk_source_set_sources = all_source_files
