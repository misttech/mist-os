# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia cc primitives.

Drop in replacements for cc_binary and cc_test:
 - fuchsia_cc_binary
 - fuchsia_cc_test

cc_binary & cc_test wrappers:
 - fuchsia_wrap_cc_binary
 - fuchsia_wrap_cc_test
"""

load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load(":fuchsia_component.bzl", "fuchsia_test_component")
load(":fuchsia_component_manifest.bzl", "fuchsia_component_manifest")
load(
    ":providers.bzl",
    "FuchsiaDebugSymbolInfo",
    "FuchsiaPackageResourcesInfo",
    "FuchsiaUnstrippedBinaryInfo",
)
load(":utils.bzl", "find_cc_toolchain", "forward_providers", "rule_variants")

KNOWN_PROVIDERS = [
    CcInfo,
    # This provider is generated by cc_binary/cc_test, but there's no way to
    # load it.
    # CcLauncherInfo,
    DebugPackageInfo,
    FuchsiaDebugSymbolInfo,
    FuchsiaPackageResourcesInfo,
    InstrumentedFilesInfo,
    OutputGroupInfo,
]

_invalid_deps_message = """Missing or mismatched exact_cc_%s_deps.
Please factor out the deps of `%s` and pass them into **both targets**.
If there are no deps, assign an empty list and pass into into both."""

def _fuchsia_cc_impl(ctx):
    # Expect exactly one binary to be generated by the native cc_* rule.
    native_outputs = ctx.attr.native_target.files.to_list()
    if len(native_outputs) != 1:
        fail("Expected exactly 1 native output for %s, got %s" % (ctx.attr.native_target, native_outputs))

    target_in = native_outputs[0]

    # Make sure we have a trailing "/"
    install_root = ctx.attr.install_root and ctx.attr.install_root.removesuffix("/") + "/"

    # Do not list the generated unstripped binary as a resource here.
    # Instead it is exposed through a FuchsiaUnstrippedBinaryInfo provider
    # which will later be processed to generate the corresponding resource
    # entry, referencing its stripped version.
    resources = []

    # Check the restricted symbols
    if ctx.attr.restricted_symbols:
        cc_toolchain = find_cc_toolchain(ctx)

        # Create a copy of the input so that we ensure that this action runs.
        # We do not want to rely on links here since they may not play well with
        # remote builds.
        target_out = ctx.actions.declare_file("_" + target_in.basename)

        ctx.actions.run(
            executable = ctx.executable._check_restricted_symbols,
            arguments = [
                "--binary",
                target_in.path,
                "--objdump",
                cc_toolchain.objdump_executable,
                "--output",
                target_out.path,
                "--restricted_symbols_file",
                ctx.file.restricted_symbols.path,
            ],
            inputs = [target_in, ctx.file.restricted_symbols],
            outputs = [target_out],
            tools = cc_toolchain.all_files,
            progress_message = "Checking that binary does not have restricted symbols %s" % target_in,
            mnemonic = "CheckRestrictedSymbols",
        )
    else:
        target_out = target_in

    # Forward CC providers along with metadata for packaging.
    return forward_providers(
        ctx,
        ctx.attr.native_target,
        rename_executable = ctx.attr._variant != "test" and ctx.attr.bin_name,
        *KNOWN_PROVIDERS
    ) + [
        ctx.attr.clang_debug_symbols[FuchsiaDebugSymbolInfo],
        FuchsiaPackageResourcesInfo(resources = resources),
        FuchsiaUnstrippedBinaryInfo(
            dest = install_root + ctx.attr.bin_name,
            unstripped_file = target_out,
        ),
    ]

_fuchsia_cc_binary, _fuchsia_cc_test = rule_variants(
    variants = (None, "test"),
    implementation = _fuchsia_cc_impl,
    toolchains = ["@bazel_tools//tools/cpp:toolchain_type"],
    doc = """Attaches fuchsia-specific metadata to native cc_* targets.

    This allows them to be directly included in fuchsia_component.
    """,
    attrs = {
        "bin_name": attr.string(
            doc = "The name of the executable to place under install_root.",
            mandatory = True,
        ),
        "install_root": attr.string(
            doc = "The path to install the built binary, defaults to bin/",
            default = "bin/",
            mandatory = False,
        ),
        "native_target": attr.label(
            doc = "The underlying cc_* target.",
            mandatory = True,
            providers = [[CcInfo, DefaultInfo], [CcSharedLibraryInfo, DefaultInfo]],
        ),
        "clang_debug_symbols": attr.label(
            doc = "Clang debug symbols.",
            mandatory = True,
            providers = [FuchsiaDebugSymbolInfo],
        ),
        "deps": attr.label_list(
            doc = """The exact list of dependencies dep-ed on by native_target.

            We need these because we can't rely on `cc_binary`'s DefaultInfo
            [run]files (Bazel does not handle static libraries correctly.)
            See https://github.com/bazelbuild/bazel/issues/1920.

            Failure to provide the *exact list* of dependencies may result in a
            runtime crash.
            """,
            providers = [[CcInfo, DefaultInfo]],
        ),
        "implicit_deps": attr.label_list(
            doc = """Implicit resources/libraries to include within the resulting package.""",
        ),
        "data": attr.label_list(
            doc = "Packaged files needed by this target at runtime.",
            providers = [FuchsiaPackageResourcesInfo],
        ),
        "restricted_symbols": attr.label(
            doc = """A file containing a list of restricted symbols.

            If provided, this list will be checked against the symbols in the binary.
            If any of the restricted symbols are present in the binary then this
            rule will fail.
            """,
            allow_single_file = True,
            mandatory = False,
        ),
        "_check_restricted_symbols": attr.label(
            default = "//fuchsia/tools:check_restricted_symbols",
            executable = True,
            cfg = "exec",
        ),
        "_cc_toolchain": attr.label(
            default = Label("@bazel_tools//tools/cpp:current_cc_toolchain"),
        ),
    } | COMPATIBILITY.FUCHSIA_ATTRS,
)

# fuchsia_cc_binary build rules.
def fuchsia_wrap_cc_binary(
        *,
        name,
        cc_binary,
        bin_name = None,
        exact_cc_binary_deps = None,
        sdk_root_label = "@fuchsia_sdk",
        clang_root_label = "@fuchsia_clang",
        features = [],
        **kwargs):
    """Wrap a native cc_binary.

    The resulting target can be used as a dep in fuchsia_component.

    Args:
        name: This target name.
        bin_name: The filename to place under bin/. Defaults to name.
        cc_binary: The existing cc_binary's target name.
        exact_cc_binary_deps: The existing cc_binary's deps. This **ALWAYS MUST BE**
            identical to cc_binary's deps to prevent runtime crashes.
            We recommend factoring out cc_binary's deps and then referencing
            them in cc_binary as well as fuchsia_wrap_cc_binary.
        sdk_root_label: Optionally override the root label of the fuchsia sdk repo.
        clang_root_label: Optionally override the root label of the fuchsia clang repo.
        features: The usual bazel meaning.
        **kwargs: Arguments to forward to the fuchsia cc_binary wrapper.
    """
    if exact_cc_binary_deps == None:
        fail(_invalid_deps_message % ("binary", cc_binary))

    data = [
        "%s//pkg/sysroot:dist" % sdk_root_label,
        "%s//:runtime" % clang_root_label,
    ]

    # Check to see if the user is requesting a static cpp compilation via our
    # provided feature. If they do not make this request we need to add the
    # dist target to include the libcxx package resources.
    # It would be tempting to use a feature_flag and add this as a select but
    # this does not work because the feature_flag will only check if the feature
    # is enabled in the context of the action which is not set yet.
    #
    # Additionally, this mechanism only works if a user specifies this feature
    # on the target they are compiling and not at a higher level since features
    # will not be known if they are set on the command line. This is not a
    # problem because we are not globally controlling this feature. If we want
    # to add that support in the future we can.
    #
    # A future optimization would be to move the select into the dist target itself.
    if "static_cpp_standard_library" not in features:
        data.append("%s//:dist" % clang_root_label)

    _fuchsia_cc_binary(
        name = name,
        bin_name = bin_name if bin_name != None else name,
        native_target = cc_binary,
        clang_debug_symbols = "%s//:debug_symbols" % clang_root_label,
        deps = exact_cc_binary_deps,
        implicit_deps = ["%s//pkg/fdio" % sdk_root_label],
        data = data,
        features = features,
        **kwargs
    )

def fuchsia_cc_binary(
        *,
        name,
        bin_name = None,
        sdk_root_label = "@fuchsia_sdk",
        clang_root_label = "@fuchsia_clang",
        tags = ["manual"],
        visibility = None,
        features = [],
        **cc_binary_kwargs):
    """A fuchsia-specific cc_binary drop-in replacement.

    The resulting target can be used as a dep in fuchsia_component.

    Args:
        name: The target name.
        bin_name: The filename to place under bin/. Defaults to name.
        sdk_root_label: Optionally override the root label of the fuchsia sdk repo.
        clang_root_label: Optionally override the root label of the fuchsia clang repo.
        tags: Tags to set for all generated targets. This type of target is marked "manual" by default.
        visibility: The visibility of all generated targets.
        features: The normal bazel meaning.
        **cc_binary_kwargs: Arguments to forward to `cc_binary`.
    """

    native.cc_binary(
        name = "_%s_native" % name,
        tags = tags + ["manual"],
        visibility = visibility,
        features = features,
        **cc_binary_kwargs
    )
    native.alias(
        name = "%s_native" % name,
        actual = "_%s_native" % name,
        tags = tags + ["manual"],
        visibility = visibility,
        features = features,
        deprecation = "fuchsia_cc_binary supports direct execution now. Please use `:%s` instead." % name,
    )
    fuchsia_wrap_cc_binary(
        name = name,
        bin_name = bin_name,
        cc_binary = "_%s_native" % name,
        exact_cc_binary_deps = cc_binary_kwargs["deps"] if "deps" in cc_binary_kwargs else [],
        sdk_root_label = sdk_root_label,
        clang_root_label = clang_root_label,
        tags = tags,
        features = features,
        visibility = visibility,
    )

# fuchsia_cc_test build rules.
def _fuchsia_cc_test_manifest_impl(ctx):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]

    # Detect googletest.
    is_gtest = False
    for dep in ctx.attr.deps:
        if dep.label.workspace_name == ctx.attr.googletest.label.workspace_name:
            is_gtest = True
            break

    # Write cml.
    generated_cml = ctx.actions.declare_file("%s.cml" % ctx.label.name)
    ctx.actions.expand_template(
        template = ctx.attr._template_file.files.to_list()[0],
        output = generated_cml,
        substitutions = {
            "{{RUNNER_SHARD}}": sdk.gtest_runner_shard if is_gtest else sdk.elf_test_runner_shard,
            "{{BINARY}}": ctx.attr.test_binary_name,
            "{{LAUNCHER_PROTOCOL}}": """{
        // Needed for ASSERT_DEATH, which is common across many unit tests.
        protocol: [ "fuchsia.process.Launcher" ],
    },""" if ctx.attr.death_unittest else "",
        },
    )

    return [
        DefaultInfo(files = depset([generated_cml])),
    ]

_fuchsia_cc_test_manifest = rule(
    implementation = _fuchsia_cc_test_manifest_impl,
    doc = """Generates a stub cml file for a given cc_test-backed _fuchsia_cc.

    Detects whether gtest is included as a dependency. If it is, the cml file
    will use gtest_runner. Otherwise it will use the elf_test_runner.
    """,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "test_binary_name": attr.string(
            doc = "The test binary's name.",
            mandatory = True,
        ),
        "deps": attr.label_list(
            doc = "The same deps passed into _fuchsia_cc.",
            mandatory = True,
            providers = [[CcInfo, DefaultInfo]],
        ),
        "googletest": attr.label(
            doc = "Any googletest label.",
            allow_single_file = True,
            mandatory = True,
        ),
        "death_unittest": attr.bool(
            doc = "Whether the test is a gtest unit test and uses ASSERT_DEATH.",
            mandatory = True,
        ),
        "_template_file": attr.label(
            doc = "The template cml file.",
            default = "//fuchsia/private:templates/cc_test_manifest.cml.tmpl",
            allow_single_file = True,
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)

def fuchsia_wrap_cc_test(
        *,
        name,
        cc_test,
        exact_cc_test_deps = None,
        sdk_root_label = "@fuchsia_sdk",
        clang_root_label = "@fuchsia_clang",
        googletest_root_label = "@com_google_googletest",
        death_unittest = False,
        tags = [],
        **kwargs):
    """Wrap a native cc_test.

    The resulting target can be used as a dep in fuchsia_component.

    Args:
        name: This target name.
        cc_test: The existing cc_test's target name.
        exact_cc_test_deps: The existing cc_test's deps. This **ALWAYS MUST BE**
            identical to cc_test's deps to prevent runtime crashes.
            We recommend factoring out cc_test's deps and then referencing
            them in cc_test as well as fuchsia_wrap_cc_test.
        sdk_root_label: Optionally override the root label of the fuchsia sdk repo.
        clang_root_label: Optionally override the root label of the fuchsia clang repo.
        googletest_root_label: Optionally override the root label of the googletest repo.
        death_unittest: Whether this test is a gtest unittest that uses ASSERT_DEATH.
        tags: Tags to set for all generated targets.
        **kwargs: Arguments to forward to the fuchsia cc_test wrapper.
    """
    if exact_cc_test_deps == None:
        fail(_invalid_deps_message % ("test", cc_test))

    _fuchsia_cc_test(
        name = name,
        bin_name = name,
        native_target = cc_test,
        clang_debug_symbols = "%s//:debug_symbols" % clang_root_label,
        deps = exact_cc_test_deps,
        implicit_deps = ["%s//pkg/fdio" % sdk_root_label],
        data = [
            "%s//pkg/sysroot:dist" % sdk_root_label,
            "%s//:dist" % clang_root_label,
            "%s//:runtime" % clang_root_label,
        ],
        tags = tags + ["manual"],
        **kwargs
    )

    _fuchsia_cc_test_manifest(
        name = "%s_autogen_cml" % name,
        test_binary_name = name,
        deps = exact_cc_test_deps,
        googletest = "%s//:BUILD.bazel" % googletest_root_label,
        death_unittest = death_unittest,
        testonly = True,
        tags = tags + ["manual"],
        **kwargs
    )

    # Generate a default component manifest.
    fuchsia_component_manifest(
        name = "%s_autogen_manifest" % name,
        component_name = name,
        src = ":%s_autogen_cml" % name,
        testonly = True,
        tags = tags + ["manual"],
        **kwargs
    )

    # Generate the default component.
    fuchsia_test_component(
        name = "%s.unittest_component" % name,
        component_name = name,
        manifest = ":%s_autogen_manifest" % name,
        deps = [
            ":%s" % name,
        ],
        tags = tags + ["manual"],
        **kwargs
    )

def fuchsia_cc_test(
        *,
        name,
        sdk_root_label = "@fuchsia_sdk",
        clang_root_label = "@fuchsia_clang",
        googletest_root_label = "@com_google_googletest",
        death_unittest = False,
        tags = ["manual"],
        visibility = None,
        **cc_test_kwargs):
    """A fuchsia-specific cc_test drop-in replacement.

    The resulting target can be used as a dep in fuchsia_component.

    Args:
        name: The target name.
        sdk_root_label: Optionally override the root label of the fuchsia sdk repo.
        clang_root_label: Optionally override the root label of the fuchsia clang repo.
        googletest_root_label: Optionally override the root label of the googletest repo.
        death_unittest: Whether this test is a gtest unittest that uses ASSERT_DEATH.
        tags: Tags to set for all generated targets. This type of target is marked "manual" by default.
        visibility: The visibility of all generated targets.
        **cc_test_kwargs: Arguments to forward to `cc_test`.
    """
    native.cc_test(
        name = "_%s_native" % name,
        tags = tags + ["manual"],
        visibility = visibility,
        **cc_test_kwargs
    )
    native.alias(
        name = "%s_native" % name,
        actual = "_%s_native" % name,
        tags = tags + ["manual"],
        visibility = visibility,
        deprecation = "fuchsia_cc_test supports direct execution now. Please use `:%s` instead." % name,
    )
    fuchsia_wrap_cc_test(
        name = name,
        cc_test = "_%s_native" % name,
        exact_cc_test_deps = cc_test_kwargs["deps"] if "deps" in cc_test_kwargs else [],
        sdk_root_label = sdk_root_label,
        clang_root_label = clang_root_label,
        googletest_root_label = googletest_root_label,
        death_unittest = death_unittest,
        tags = tags,
        visibility = visibility,
    )

#TODO: do not couple the generated component with the the native_cc component.
# Rather the "name" should remain on the native.cc_test target and we should
# establish a convention for the fuchsia_unittest_package that the unittests
# that are passed in must create a target called <name>.unittest_component which
# it then pulls in. This way, if a user wants to create their own test component
# they can use fuchsia_test_package and the generated target will be ignored.
