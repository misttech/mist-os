load(
    "@fuchsia_sdk//fuchsia:defs.bzl",
    "fuchsia_bind_cc_library",
    "fuchsia_fidl_library",
    "fuchsia_fidl_bind_library",
    "fuchsia_fidl_hlcpp_library",
    "fuchsia_fidl_llcpp_library",
)

fuchsia_fidl_library(
    name = "{{name}}",
    srcs = [
        {{sources}}
    ],
    cc_bindings = [
        "cpp",
        "cpp_wire",
        "cpp_driver_wire",
        "cpp_driver",
    ],
    library = "{{name}}",
    target_api_level = "HEAD",
    deps = [
        {{deps}}
    ],
    sdk_for_default_deps = "{{parent_sdk}}",
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)

fuchsia_fidl_bind_library(
    name = "{{name}}_bindlib",
    library = ":{{name}}",
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)

fuchsia_bind_cc_library(
    name = "{{name}}_bindlib_cc",
    library = ":{{name}}_bindlib",
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)

# LEGACY: This target will soon be deprecated
fuchsia_fidl_hlcpp_library(
    name = "{{name}}_cc",
    library = ":{{name}}",
    deps = [
        "{{parent_sdk}}//pkg/fidl_cpp",
        {{cc_deps}}
    ],
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)

# TODO(fxbug.dev/117103): Rename HLCPP generated bindings to "_hlcpp" suffix.
alias(
    name = "{{name}}_hlcpp",
    actual = ":{{name}}_cc",
)

# LEGACY: This target will soon be deprecated
fuchsia_fidl_llcpp_library(
    name = "{{name}}_llcpp_cc",
    library = ":{{name}}",
    deps = [
        "{{parent_sdk}}//pkg/fidl_cpp_v2",
        "{{parent_sdk}}//pkg/fidl_cpp_wire",
        {{llcpp_deps}}
    ],
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)
