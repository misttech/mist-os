cc_library(
    name = "{{name}}",
    srcs = [
        {{sources}}
    ],
    hdrs = [
        {{headers}}
    ],
    deps = [
        {{deps}}
        {{fidl_deps}}
    ] + [{{fidl_llcpp_deps}}],
    strip_include_prefix = "{{relative_include_dir}}",
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)
