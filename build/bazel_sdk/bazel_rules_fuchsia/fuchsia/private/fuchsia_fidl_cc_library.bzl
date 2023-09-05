# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A cc_library backed by a FIDL library."""

load(":providers.bzl", "FuchsiaFidlLibraryInfo")

_CodegenInfo = provider("Carries generated information across FIDL bindings code generation ", fields = ["files"])

def _typed_deps(deps, binding_type):
    result = []
    for dep in deps:
        if ":" in dep:  # appends the binding_type suffix
            dep_with_binding = get_cc_lib_name(dep, binding_type)
        else:
            # assumption: this is a path with an implicit target name
            # TODO: THIS WILL FAIL eventually with local targets without ":" for example.
            # A better implementation is needed! We can't use Label() here because this is evaluated
            # in the generator context, not the SDK context.
            # One possible better implementation is to make fuchsia_fidl carry all the transitive dependencies per
            # cc binding type, and provide them as a Provider, and then just collect them in _codegen and make them
            # _codegen's own dependencies, which would probably cause native.cc_library to do the right thing.
            dep_label = dep.rpartition("/")[2]
            dep_label = get_cc_lib_name(dep_label, binding_type)
            dep_with_binding = "%s:%s" % (dep, dep_label)

        result.append(dep_with_binding)
    return result

def get_cc_lib_name(fidl_target_name, binding_type):
    return "{fidl}_{binding_type}".format(fidl = fidl_target_name, binding_type = binding_type)

def fuchsia_fidl_cc_library(name, library, binding_type = "cpp_wire", sdk_for_default_deps = None, deps = [], tags = [], **kwargs):
    """Generates cc_library() for the given fidl_library.

    Args:
      name: Target name. Required.
      library: fidl_library() target to generate the language bindings for. Required.
      binding_type: the FIDL binding type, for example "cpp_wire", "cpp_driver" or "cpp_natural_types". Defaults to "cpp_wire"
      sdk_for_default_deps: Name of the Bazel workspace where default FIDL library dependencies are defined. If empty or not defined, defaults to the current repository.
      deps: Additional dependencies.
      tags: Optional tags.
      **kwargs: Remaining args.
    """
    gen_name = "%s_codegen" % name
    impl_name = "%s_impl" % name

    _codegen(
        name = gen_name,
        library = library,
        binding_type = binding_type,
        sdk_for_default_deps = sdk_for_default_deps,
    )

    _impl_wrapper(
        name = impl_name,
        codegen = ":%s" % gen_name,
    )

    native.cc_library(
        name = name,
        hdrs = [
            ":%s" % gen_name,
        ],
        srcs = [
            ":%s" % impl_name,
            # For the coding tables.
            library,
        ],
        # This is necessary in order to locate generated headers.
        strip_include_prefix = gen_name + "." + binding_type,
        deps = _typed_deps(deps, binding_type) + _get_binding_info(sdk_for_default_deps, binding_type)["deps"],
        tags = tags,
        **kwargs
    )

def _codegen_impl(context):
    sdk = context.toolchains["@fuchsia_sdk//fuchsia:toolchain"]

    ir = context.attr.library[FuchsiaFidlLibraryInfo].ir
    name = context.attr.library[FuchsiaFidlLibraryInfo].name

    base_path = context.attr.name + "." + context.attr.binding_type

    # This declaration is needed in order to get access to the full path.
    root = context.actions.declare_directory(base_path)

    b = _get_binding_info(context.attr.sdk_for_default_deps, context.attr.binding_type)
    header_files = []
    header_files.extend(b["headers"])
    source_files = []
    source_files.extend(b["sources"])
    for layer_dep in b["layer_deps"]:
        layer_dep_b = _get_binding_info(context.attr.sdk_for_default_deps, layer_dep)
        header_files.extend(layer_dep_b["headers"])
        source_files.extend(layer_dep_b["sources"])

    # TODO(fxbug.dev/108680): Better workaround for skipping codegen for zx.
    if name == "zx":
        source_files = ["markers.h"]

    dir = base_path + "/fidl/" + name + "/cpp"

    headers = []
    sources = []
    for header in header_files:
        headers.append(context.actions.declare_file(dir + "/" + header))
    for source in source_files:
        sources.append(context.actions.declare_file(dir + "/" + source))

    outputs = [root] + headers + sources
    context.actions.run(
        executable = sdk.fidlgen_cpp,
        arguments = [
            "--json",
            ir.path,
            "--root",
            root.path,
        ],
        inputs = [
            ir,
        ],
        outputs = outputs,
        mnemonic = "FidlGenCc",
    )

    return [
        _CodegenInfo(files = depset(sources)),
        DefaultInfo(files = depset(headers)),
    ]

# Runs fidlgen to produce both the header file and the implementation file.
# Only exposes the header as a source, as the two files need to be consumed by
# the cc_library as two separate rules.
_codegen = rule(
    implementation = _codegen_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    # Files must be generated in genfiles in order for the header to be included
    # anywhere.
    output_to_genfiles = True,
    attrs = {
        "library": attr.label(
            doc = "The FIDL library to generate code for",
            mandatory = True,
            allow_files = False,
            providers = [FuchsiaFidlLibraryInfo],
        ),
        "binding_type": attr.string(
            doc = "Type of bindings to expose",
            mandatory = True,
        ),
        "sdk_for_default_deps": attr.string(
            doc = "Name of the Bazel workspace where default FIDL library dependencies are defined. If empty or not defined, defaults to @fuchsia_sdk.",
            mandatory = False,
        ),
    },
)

def _impl_wrapper_impl(context):
    files = context.attr.codegen[_CodegenInfo].files
    return [DefaultInfo(files = files)]

# Simply declares the implementation file generated by the codegen target as an
# output.
# This allows the implementation file to be exposed as a source in its own rule.
_impl_wrapper = rule(
    implementation = _impl_wrapper_impl,
    output_to_genfiles = True,
    attrs = {
        "codegen": attr.label(
            doc = "The codegen rules generating the implementation file",
            mandatory = True,
            allow_files = False,
            providers = [_CodegenInfo],
        ),
    },
)

def _get_binding_info(sdk_for_default_deps, binding_type):
    # Note: deps needs to be flattened, since Starlark does not support
    # recursivity or unbounded loops.
    if not sdk_for_default_deps:
        sdk_for_default_deps = "@fuchsia_sdk"

    wire_dep = sdk_for_default_deps + "//pkg/fidl_cpp_wire"
    natural_dep = sdk_for_default_deps + "//pkg/fidl_cpp_v2"
    driver_dep = sdk_for_default_deps + "//pkg/fidl_driver"
    driver_natural_dep = sdk_for_default_deps + "//pkg/fidl_driver_natural"

    bindings = {
        "cpp_common": {
            "headers": ["common_types.h", "markers.h"],
            "sources": ["common_types.cc"],
            "layer_deps": [],
            "deps": [wire_dep],
        },
        "cpp_wire_types": {
            "headers": ["wire_types.h"],
            "sources": ["wire_types.cc"],
            "layer_deps": ["cpp_common"],
            "deps": [],
        },
        "cpp_wire": {
            "headers": ["wire.h", "wire_messaging.h"],
            "sources": ["wire_messaging.cc"],
            "layer_deps": ["cpp_wire_types", "cpp_common"],
            "deps": [wire_dep],
        },
        "cpp_wire_testing": {
            "headers": ["wire_test_base.h"],
            "sources": [""],
            "layer_deps": ["cpp_wire", "cpp_wire_types", "cpp_common"],
            "deps": [],
        },
        "cpp_natural_types": {
            "headers": ["natural_types.h"],
            "sources": ["natural_types.cc"],
            "layer_deps": ["cpp_common"],
            "deps": [],
        },
        "cpp_natural_ostream": {
            "headers": ["natural_ostream.h"],
            "sources": ["natural_ostream.cc"],
            "layer_deps": ["cpp_natural_types", "cpp_common"],
            "deps": [],
        },
        "cpp_type_conversions": {
            "headers": ["type_conversions.h"],
            "sources": ["type_conversions.cc"],
            "layer_deps": ["cpp_natural_types", "cpp_wire", "cpp_common", "cpp_wire_types"],
            "deps": [],
        },
        "cpp": {
            "headers": ["fidl.h", "natural_messaging.h"],
            "sources": ["natural_messaging.cc"],
            "layer_deps": ["cpp_natural_types", "cpp_type_conversions", "cpp_wire", "cpp_common", "cpp_wire_types"],
            "deps": [wire_dep, natural_dep],
        },
        "cpp_driver_wire": {
            "headers": ["driver/wire.h", "driver/wire_messaging.h"],
            "sources": ["driver/wire_messaging.cc"],
            "layer_deps": ["cpp_wire_types", "cpp_common"],
            "deps": [wire_dep, driver_dep],
        },
        "cpp_driver": {
            "headers": ["driver/wire.h", "driver/wire_messaging.h", "natural_messaging.h", "driver/natural_messaging.h"],
            "sources": ["driver/wire_messaging.cc", "natural_messaging.cc", "driver/natural_messaging.cc"],
            "layer_deps": ["cpp_wire_types", "cpp_common", "cpp_natural_types", "cpp_type_conversions", "cpp_wire"],
            "deps": [wire_dep, natural_dep, driver_dep, driver_natural_dep],
        },
        "cpp_hlcpp_conversion": {
            "headers": ["hlcpp_conversion.h"],
            "sources": [""],
            "layer_deps": ["cpp_natural_types", "cpp_common"],
            "deps": [],
        },
    }

    if binding_type not in bindings:
        fail("Unsupported binding type: %s" % binding_type)
    return bindings[binding_type]
