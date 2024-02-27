"""Implementation of cc_bind_rules rule"""

load(":providers.bzl", "FuchsiaBindLibraryInfo", "FuchsiaPackageResourcesInfo")
load(":utils.bzl", "make_resource_struct")

def _process_bindc_args(context):
    # Collect all the bind files and their filepaths that will be passed to bindc.
    inputs = []
    include_filepaths = []

    for dep in context.attr.deps:
        trans_srcs = dep[FuchsiaBindLibraryInfo].transitive_sources
        for src in trans_srcs.to_list():
            # Only add unique instances.
            if src.path in include_filepaths:
                continue
            inputs.append(src)
            if len(include_filepaths) == 0:
                include_filepaths.append("--include")

            include_filepaths.append(src.path)

    files_argument = []
    for file in context.files.rules:
        inputs.append(file)
        files_argument.append(file.path)
    return {
        "inputs": inputs,
        "files_argument": files_argument,
        "include_filepaths": include_filepaths,
    }

def _fuchsia_driver_bind_bytecode_impl(context):
    args = _process_bindc_args(context)
    sdk = context.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    context.actions.run(
        executable = sdk.bindc,
        arguments = [
                        "compile",
                    ] + args["include_filepaths"] +
                    [
                        "--output",
                        context.outputs.output.path,
                    ] + args["files_argument"],
        inputs = args["inputs"],
        outputs = [
            context.outputs.output,
        ],
        mnemonic = "Bindcbc",
    )
    return [
        FuchsiaPackageResourcesInfo(
            resources = [make_resource_struct(
                src = context.outputs.output,
                dest = "meta/bind/" + context.outputs.output.basename,
            )],
        ),
    ]

fuchsia_driver_bind_bytecode = rule(
    implementation = _fuchsia_driver_bind_bytecode_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "rules": attr.label(
            doc = "Path to the bind rules source file",
            mandatory = True,
            allow_single_file = True,
        ),
        "output": attr.output(
            mandatory = True,
        ),
        "deps": attr.label_list(
            doc = "The list of libraries this library depends on",
            mandatory = False,
            providers = [FuchsiaBindLibraryInfo],
        ),
    },
)
