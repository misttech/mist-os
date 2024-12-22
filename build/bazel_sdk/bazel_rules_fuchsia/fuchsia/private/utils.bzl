# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Common utilities needed by Bazel SDK rules."""

load(":providers.bzl", "FuchsiaProvidersInfo")

_INVALID_LABEL_CHARACTERS = "\"!%@^_#$&'()*+,;<=>?[]{|}~/".elems()

def _fuchsia_cpu_alias(cpu):
    if cpu == "aarch64":
        return "arm64"
    return cpu

def fuchsia_cpu_from_ctx(ctx):
    """ Returns the Fuchsia CPU for the given rule invocation. """
    target_cpu = ctx.var["TARGET_CPU"]
    return _fuchsia_cpu_alias(target_cpu)

def normalized_target_name(label):
    label = label.lower()
    for c in _INVALID_LABEL_CHARACTERS:
        label = label.replace(c, ".")
    return label

def label_name(label):
    # convert the label to a single word
    # //foo/bar -> bar
    # :bar -> bar
    # //foo:bar -> bar
    return label.split("/")[-1].split(":")[-1]

def append_suffix_to_label(label_str, suffix, separator = "."):
    """ Canonicalizes a label given at the macro-level and appends a suffix.

    foo -> foo.bar
    :foo -> :foo.bar
    //src/foo:foo -> //src/foo:foo.bar
    //src/foo:baz -> //src/foo:baz.bar
    //src/foo -> //src/foo:foo.bar

    Args:
        label_str: The label string to append to.
        suffix: The suffix to append.
        separator: An optional string to use as a separator.
    Returns:
        The new name with the suffix appended
    """
    unqualified_name = separator.join([label_name(label_str), suffix])
    if label_str.startswith(":"):
        return ":{}".format(unqualified_name)
    elif label_str.startswith("//"):
        return "{}:{}".format(label_str.split(":")[0], unqualified_name)
    else:
        return unqualified_name

def get_project_execroot(ctx):
    # Gets the project/workspace execroot relative to the output base.
    # See https://bazel.build/docs/output_directories.
    return "execroot/%s" % ctx.workspace_name

def get_target_execroot(ctx, target):
    # Gets the execroot for a given target, relative to the project execroot.
    # See https://bazel.build/docs/output_directories.
    return target[DefaultInfo].files_to_run.runfiles_manifest.dirname + "/" + ctx.workspace_name

def stub_executable(ctx):
    # buildifier: disable=function-docstring-args
    # buildifier: disable=function-docstring-return
    """Returns a stub executable that fails with a message."""
    executable_file = ctx.actions.declare_file(ctx.label.name + "_fail.sh")
    content = """#!/bin/bash
    echo "---------------------------------------------------------"
    echo "ERROR: Attempting to run a target or dependency that is not runnable"
    echo "Got {target}"
    echo "---------------------------------------------------------"
    exit 1
    """.format(target = ctx.attr.name)

    ctx.actions.write(
        output = executable_file,
        content = content,
        is_executable = True,
    )

    return executable_file

def flatten(elements):
    # buildifier: disable=function-docstring-args
    # buildifier: disable=function-docstring-return
    """Flattens an arbitrarily nested list of lists to non-list elements while preserving order."""
    result = []
    unprocessed = list(elements)
    for _ in range(len(str(unprocessed))):
        if not unprocessed:
            return result
        elem = unprocessed.pop(0)
        if type(elem) in ("list", "tuple"):
            unprocessed = list(elem) + unprocessed
        else:
            result.append(elem)
    fail("Unable to flatten list!")

def collect_runfiles(ctx, *elements, ignore_types = []):
    # buildifier: disable=function-docstring-args
    # buildifier: disable=function-docstring-return
    """Collects multiple types of elements (...files, ...targets, ...runfiles) into runfiles."""

    # Map to runfiles objects.
    runfiles = []
    for elem in flatten(elements):
        if type(elem) == "Target":
            runfiles.append(elem[DefaultInfo].default_runfiles)
            files_to_run = elem[DefaultInfo].files_to_run
            if files_to_run.executable and files_to_run.runfiles_manifest:
                runfiles.append(ctx.runfiles([
                    files_to_run.executable,
                    files_to_run.runfiles_manifest,
                ]))
        elif type(elem) == "File":
            runfiles.append(ctx.runfiles([elem]))
        elif type(elem) == "runfiles":
            runfiles.append(elem)
        elif type(elem) not in ignore_types:
            fail("Unable to get runfiles from %s: %s" % (type(elem), str(elem)))

    # Merges runfiles for a given target.
    return ctx.runfiles().merge_all(runfiles)

def wrap_executable(ctx, executable, *arguments, script_name = None):
    # buildifier: disable=function-docstring-args
    # buildifier: disable=function-docstring-return
    """Wraps an executable with predefined command line arguments.

    Creates a wrapper script that invokes an underlying executable with
    predefined command line arguments.

    script_name defaults to `run_${target_name}.sh`.
    """
    wrapper = ctx.actions.declare_file(script_name or "run_%s.sh" % ctx.attr.name)

    # Convert file arguments into strings and serialize arguments.
    def serialize(arg):
        readlink = False
        if type(arg) == "Target":
            arg = arg[DefaultInfo].files_to_run.executable
            readlink = True
        if type(arg) == "File":
            arg = arg.short_path
        arg = "'%s'" % arg.replace("'", "\\'")

        # Follow symlink for complex tool executables, otherwise we will run
        # into issues with nested runfiles symlink farms.
        if readlink:
            arg = "$(readlink -f %s)" % arg
        return arg

    command = [serialize(arg) for arg in [executable] + list(arguments)]

    ctx.actions.write(wrapper, """#!/bin/bash
%s $@
""" % " ".join(command), is_executable = True)
    return wrapper, collect_runfiles(ctx, executable, arguments, ignore_types = ["string"])

def _add_providers_info(implementation):
    """Wrap a rule implementation function to add a FuchsiaProvidersInfo provider.

    Args:
       implementation: A rule implementation function, i.e. a callable object that
          takes a single rule 'ctx' value as argument and returns a list of
          providers.

    Returns:
       A new rule implementation function / callable object, which returns the
       result of calling 'implementation(ctx)', after potentially appending a
       FuchsiaProvidersInfo provider to it.
    """

    def _impl(ctx):
        return track_providers(implementation(ctx))

    return _impl

def _add_default_executable(implementation):
    """Wrap a rule implementation function to add a default stub executable if needed.

    This returns a new callable object that acts as a rule implementation function,
    i.e. it accepts a single 'ctx' rule context argument, and will first call
    'implementation(ctx)' with it to retrieve a list of provider values.

    If that list does not include a DefaultInfo value, a new one will be appended,
    which points to a stub shell script executable. The script will print an error
    message at build time to indicate that the target is not really executable.

    Used internally by rule_variants(), see related documentation for more details.

    Args:
       implementation: A rule implementation function, i.e. a callable object that
          takes a single rule 'ctx' value as argument and returns a list of
          providers.

    Returns:
       A new rule implementation function / callable object, which returns the
       result of calling 'implementation(ctx)', after potentially appending a
       DefaultInfo value to it.
    """

    def _impl(ctx):
        providers = implementation(ctx)
        if not [provider for provider in providers if type(provider) == "DefaultInfo"]:
            providers.append(DefaultInfo(executable = stub_executable(ctx)))
        return providers

    return _impl

def rule_variants(implementation, variants = [], attrs = {}, **rule_kwargs):
    """Creates variants of a rule.

    Creates one or more rule() objects whose attributes vary slightly based on
    the value of items in the 'variants' input list, and which share a common
    implementation function.

    Example usage:

       ```
       def _foo_impl(ctx):
           ....

       foo_binary, foo_test = rule_variants(
            _foo_impl, ["executable", "test"], attrs = {...})
       ```

    Args:
        implementation: base rule implementation function used by all
           result rule instances. Must take a single 'ctx' argument.

        variants: A list of variant, each item can be None or a string
           describing a non-default variant. Valid values are:

           - None: Create a rule() that uses 'implementation' and 'attrs' directly.
                This sets both 'executable' and 'test' to False.

           - "executable": Create a rule() that uses an implementation function
                that calls 'implementation' then looks at its result, and will
                add a stub executable *if* it does not include an executable in
                its DefaultInfo value (this is used to print an error message
                at build or run time to tell the user the target is not really
                executable). Sets 'executable' to True, and 'test' to False.

            - "test": Similar to "executable" but sets 'test' to True, making
                the target usable with `bazel test`.

        attrs: A rule() 'attrs' dictionary value, providing common attributes
           for all result rule instances. For each result rule() value, this will
           be augmented with a private '_variant' attribute corresponding to
           the 'variants' item value used to create it.

        **rule_kwargs: Extra arguments are passed directly to each result rule()
           constructor.

    Returns:
        A list of rule() instances, one per item in 'variants'.
    """
    return [rule(
        executable = variant == "executable",
        test = variant == "test",
        attrs = dict(attrs, _variant = attr.string(default = variant or "")),
        implementation = _add_providers_info(
            implementation if variant == None else _add_default_executable(implementation),
        ),
        **rule_kwargs
    ) for variant in variants]

def rule_variant(implementation, variant = None, attrs = {}, **rule_kwargs):
    """Creates a variant of a rule. See rule_variants for argument descriptions."""
    return rule_variants(variants = [variant], attrs = attrs, implementation = implementation, **rule_kwargs)[0]

def track_providers(providers):
    return providers + [FuchsiaProvidersInfo(
        providers = [
            provider
            for provider in providers
            if type(provider) != "DefaultInfo"
        ],
    )]

def can_forward_provider(provider):
    """Return True if a given provider value should never be forwarded to dependents.

    This is important for providers that are collected through aspects, as forwarding
    them (e.g. with a function like forward_providers()), would create
    duplicate entries in the build graph, resulting in chaos.

    Unlike native providers like DefaultInfo or CCInfo, `type(provider)` will always
    return "struct" for custom providers, so instead rely on the fact that their values
    include a "never_forward" field which should be True if they should not be
    forwarded.

    Args:
       provider: A provider value.
    Returns:
       True if the value can be forwarded to dependents by fowrard_providers().
    """
    return not getattr(provider, "never_forward", False)

# buildifier: disable=function-docstring
def forward_providers(ctx, target, *providers, rename_executable = None):
    default_info = target[DefaultInfo]
    if default_info.files_to_run and default_info.files_to_run.executable:
        executable = default_info.files_to_run.executable
        executable_symlink = ctx.actions.declare_file(
            rename_executable or "_" + executable.basename,
        )
        ctx.actions.symlink(
            output = executable_symlink,
            target_file = executable,
            is_executable = True,
        )
        default_info = DefaultInfo(
            files = depset([executable_symlink] + [
                file
                for file in default_info.files.to_list()
                if file != executable
            ]) if rename_executable else default_info.files,
            runfiles = default_info.default_runfiles,
            executable = executable_symlink,
        )
    target_provider_info = target[FuchsiaProvidersInfo] if (
        FuchsiaProvidersInfo in target
    ) else struct(providers = [])
    return [
        target[Provider]
        for Provider in providers
        if Provider in target and can_forward_provider(Provider)
    ] + target_provider_info.providers + [default_info]

def _forward_providers(ctx):
    return forward_providers(ctx, ctx.attr.actual)

_alias, _alias_for_executable, _alias_for_test = rule_variants(
    variants = (None, "executable", "test"),
    implementation = _forward_providers,
    attrs = {
        "actual": attr.label(
            doc = "The test workflow entity target to alias.",
            providers = [FuchsiaProvidersInfo],
            mandatory = True,
        ),
    },
)

def alias(*, name, executable, testonly = False, **kwargs):
    # buildifier: disable=function-docstring-header
    """
    We have to create our own alias macro because Bazel is unreasonable:
    https://github.com/bazelbuild/bazel/issues/10893

    The underlying target must be created with `rule_variant(s)` or manually
    include `FuchsiaProvidersInfo` in order to forward providers.
    """
    return ((
        _alias_for_test if testonly else _alias_for_executable
    ) if executable else _alias)(
        name = name,
        testonly = testonly,
        **kwargs
    )

def get_target_deps_from_attributes(rule_attr, rule_kind = None, known_rule_kinds = {}):
    """Return all dependencies from a given target context during analysis.

    Args:
        rule_attr: The ctx.attr value for the current target.
        rule_kind: Optional string for the rule kind (this is aspect_ctx.rule.kind
             when called from an aspect implementation function). If provided,
             this can speed up the computation for a few known target kinds.
        known_rule_kinds: Optional dictionary containing known rule kinds and their attributes to check
    Returns:
        A list of Target values corresponding to the dependencies of the current
        target.
    """
    attr_names = known_rule_kinds.get(rule_kind)
    if not attr_names:
        # For unknown rule kinds, parse all attributes and filter
        # those that are Targets or lists of Targets to the result.
        attr_names = dir(rule_attr)

    result = []
    for attr_name in attr_names:
        attr_value = getattr(rule_attr, attr_name, None)
        if not attr_value:
            continue
        if type(attr_value) == "Target":
            result.append(attr_value)
            continue
        if type(attr_value) == "list" and type(attr_value[0]) == "Target":
            result.extend(attr_value)
            continue

    return depset(result).to_list()

def filter(obj, value = None, exclude = True):
    # buildifier: disable=function-docstring-args
    # buildifier: disable=function-docstring-return
    """Recursively removes matching fields/elements from an object by mutating."""
    if type(obj) not in ("dict", "list"):
        fail("Unsupported data type.")

    nested_fields = [obj]

    # Since dictionaries and lists can be represented as DAGs, this represents
    # one filter operation within an iterative BFS.
    def filter_next():
        obj = nested_fields.pop(0)

        # Lists and dictionaries can both be represented as key-value pairs.
        for k, nested in (obj.items() if type(obj) == "dict" else enumerate(obj)):
            if type(nested) in ("dict", "list"):
                # Add a nested object to the BFS queue.
                nested_fields.append(nested)
            elif (nested == value) == exclude:
                # Remove the matching value's field by mutating the object.
                obj.pop(k)

    # Using and iterative BFS to filter all matching values within `obj` should
    # take less than `len(str(obj))` iterations.
    for _ in range(len(str(obj))):
        # Empty nested_fields means that we're done with our BFS.
        if not nested_fields:
            return obj
        filter_next()

    # In case the previous assumption is violated.
    fail("Unable to filter all none values!")

def make_resource_struct(src, dest):
    return struct(
        src = src,
        dest = dest,
    )

def get_runfiles(target):
    # Helper function to get the runfiles as a list of files from a target.
    return [symlink.target_file for symlink in target[DefaultInfo].default_runfiles.root_symlinks.to_list()]

# Libs all end with .so or .so followed by a semantic version.
# Examples: libname.so, libname.so.1, libname.so.1.1
# buildifier: disable=function-docstring
def is_lib(file):
    rparts = file.basename.rpartition(".so")
    if (rparts[1] != ".so"):
        return False
    for char in rparts[2].elems():
        if not (char.isdigit() or char == "."):
            return False
    return True

#TODO(b/341799247) The logic for find_cc_toolchain is copied from
#https://cs.opensource.google/fuchsia/fuchsia/+/main:third_party/bazel_rules_cc/cc/find_cc_toolchain.bzl;l=56;drc=13d212d39bbc415fd971138396cfd99320e04517
#
# We need to do this because we are currently using an older version of rules_cc
# that is not compatible with our current version of bazel. Once we roll rules_cc
# we can go back to using the method defined there.
CC_TOOLCHAIN_TYPE = "@bazel_tools//tools/cpp:toolchain_type"

def find_cc_toolchain(ctx):
    """
Returns the current `CcToolchainInfo`.

    Args:
      ctx: The rule context for which to find a toolchain.

    Returns:
      A CcToolchainInfo.
    """

    # Check the incompatible flag for toolchain resolution.
    if hasattr(cc_common, "is_cc_toolchain_resolution_enabled_do_not_use") and cc_common.is_cc_toolchain_resolution_enabled_do_not_use(ctx = ctx):
        if not CC_TOOLCHAIN_TYPE in ctx.toolchains:
            fail("In order to use find_cc_toolchain, your rule has to depend on C++ toolchain. See find_cc_toolchain.bzl docs for details.")
        toolchain_info = ctx.toolchains[CC_TOOLCHAIN_TYPE]
        if toolchain_info == None:
            # No cpp toolchain was found, so report an error.
            fail("Unable to find a CC toolchain using toolchain resolution. Target: %s, Platform: %s, Exec platform: %s" %
                 (ctx.label, ctx.fragments.platform.platform, ctx.fragments.platform.host_platform))
        if hasattr(toolchain_info, "cc_provider_in_toolchain") and hasattr(toolchain_info, "cc"):
            return toolchain_info.cc
        return toolchain_info

    # Fall back to the legacy implicit attribute lookup.
    if hasattr(ctx.attr, "_cc_toolchain"):
        return ctx.attr._cc_toolchain[cc_common.CcToolchainInfo]

    # We didn't find anything.
    fail("In order to use find_cc_toolchain, your rule has to depend on C++ toolchain. See find_cc_toolchain.bzl docs for details.")

def with_fuchsia_constraint(target_compatible_with = []):
    """Adds "@platforms//os:fuchsia" to the specified list if it's not already present."""
    return target_compatible_with if (
        "@platforms//os:fuchsia" in target_compatible_with
    ) else target_compatible_with + ["@platforms//os:fuchsia"]

# These rule attributes are needed by rules that want to call preprocess_file()
PREPROCESS_FILE_ATTRS = {
    "_preprocessor_binary": attr.label(
        default = Label("@fuchsia_clang//:bin/clang-cpp"),
        executable = True,
        allow_single_file = True,
        cfg = "exec",
    ),
}

def preprocess_file(ctx, source, includes, headers = [], files = depset()):
    """Helper function for .S file preprocessing.

    This function must be called from a rule implementation function
    whose rule definitions' attributes includes PREPROCESS_FILE_ATTRS

    Args:
      ctx: The rule context.
      source: The source file to be preprocessed. It should be a .S file
      includes: A depset of includes paths (as strings) needed for preprocessing.
      headers: A list of headers in the format of File.
      files: A depset of additional inputs to preprocessing action.

    Returns:
      A File of preprocessed file.
    """

    dest_filename = source.basename.rstrip(".S")
    output_file = ctx.actions.declare_file(dest_filename)

    pp_args = ctx.actions.args()
    pp_args.add("-undef")
    pp_args.add("-P")
    pp_args.add_all(includes, before_each = "-I")
    pp_args.add("-x", "assembler-with-cpp")
    pp_args.add(source)
    pp_args.add("-o", output_file)

    ctx.actions.run(
        executable = ctx.executable._preprocessor_binary,
        arguments = [pp_args],
        inputs = [source] + headers + files.to_list(),
        outputs = [output_file],
    )
    return output_file
