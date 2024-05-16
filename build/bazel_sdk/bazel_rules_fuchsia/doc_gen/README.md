# Documentation Generation

This directory contains a bzl_library which exposes the public interfaces
for the Fuchsia Bazel Rules. This target is used during doc creation to create
documentation.

If you need to add a new top-level bzl file it needs to be added to the
`//doc_gen:public_bzl_library` target. If you are just adding new interfaces
to an existing library then they will automatically be added to the list of
symbols that are exported.

If you need to add a new private directory to the source tree you will likely
need to add a new bzl_library which exports the bzl files and any other deps
that are loaded. This library needs to be made visible to the //doc_gen:__pkg__
target and added to the //doc_gen:public_bzl_library target's deps.

```python
bzl_library(
    name = "starlark_files",
    srcs =
        glob(["*.bzl"]) + [
            "@rules_license//rules:standard_package",
            "@rules_license//rules_gathering:standard_package",
        ],
    visibility = [
        "//doc_gen:__pkg__",
    ],
)
```
