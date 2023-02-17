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
    ] + select({
        "//:has_experimental": [{{fidl_llcpp_deps}}],
        "//:no_experimental": [],
    }),
    strip_include_prefix = "{{relative_include_dir}}",
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)
