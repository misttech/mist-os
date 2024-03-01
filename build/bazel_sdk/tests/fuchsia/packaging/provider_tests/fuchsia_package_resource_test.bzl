# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")
load("@fuchsia_sdk//fuchsia:defs.bzl", "fuchsia_package_resource", "fuchsia_package_resource_collection")
load("@fuchsia_sdk//fuchsia/private:providers.bzl", "FuchsiaPackageResourcesInfo")

## Provider Tests

def _provider_contents_test_impl(ctx):
    env = analysistest.begin(ctx)

    target_under_test = analysistest.target_under_test(env)
    resources = target_under_test[FuchsiaPackageResourcesInfo].resources

    # make sure all counts are the same
    asserts.equals(env, len(ctx.attr.expected_dests), len(ctx.attr.expected_srcs))
    asserts.equals(env, len(ctx.attr.expected_dests), len(resources))

    for idx in range(0, len(resources)):
        resource = resources[idx]

        # verify that our src is set correctly
        asserts.equals(
            env,
            ctx.attr.expected_srcs[idx],
            resource.src.path,
        )

        # verify that the dest is set correctly
        asserts.equals(
            env,
            ctx.attr.expected_dests[idx],
            resource.dest,
        )

    return analysistest.end(env)

provider_contents_test = analysistest.make(
    _provider_contents_test_impl,
    attrs = {
        "expected_dests": attr.string_list(),
        "expected_srcs": attr.string_list(),
    },
)

def _test_provider_contents():
    expected_dest = "/data/foo"

    # Note, the expected_src needs to be in sync with the location of this file.
    expected_src = "fuchsia/packaging/provider_tests/text_file.txt"

    # Rule under test.
    fuchsia_package_resource(
        name = "provider_contents_subject",
        dest = expected_dest,
        src = ":text_file.txt",
        tags = ["manual"],
    )

    # Testing rule.
    provider_contents_test(
        name = "provider_contents_test",
        target_under_test = ":provider_contents_subject",
        expected_dests = [expected_dest],
        expected_srcs = [expected_src],
    )

def _test_multiple_resources():
    expected_dests = ["/data/foo", "/data/bar"]

    # Note, the expected_src needs to be in sync with the location of this file.
    expected_src = "fuchsia/packaging/provider_tests/text_file.txt"

    fuchsia_package_resource(
        name = "resource_1",
        dest = expected_dests[0],
        src = ":text_file.txt",
        tags = ["manual"],
    )

    fuchsia_package_resource(
        name = "resource_2",
        dest = expected_dests[1],
        src = ":text_file.txt",
        tags = ["manual"],
    )

    # Rule under test.
    fuchsia_package_resource_collection(
        name = "multi_provider_contents_subject",
        resources = [":resource_1", ":resource_2"],
        tags = ["manual"],
    )

    # Testing rule.
    provider_contents_test(
        name = "multi_provider_contents_test",
        target_under_test = ":multi_provider_contents_subject",
        expected_dests = expected_dests,
        expected_srcs = [expected_src, expected_src],
    )

## Failure Tests

def _failure_testing_test_impl(ctx):
    env = analysistest.begin(ctx)
    asserts.expect_failure(env, ctx.attr.expected_failure_message)
    return analysistest.end(env)

failure_testing_test = analysistest.make(
    _failure_testing_test_impl,
    expect_failure = True,
    attrs = {
        "expected_failure_message": attr.string(),
    },
)

def _test_empty_dest_failure():
    fuchsia_package_resource(
        name = "empty_dest_should_fail",
        dest = "",
        src = ":text_file.txt",
        tags = ["manual"],
    )

    failure_testing_test(
        name = "empty_dest_should_fail_test",
        target_under_test = ":empty_dest_should_fail",
        expected_failure_message = "dest must not be an empty string",
    )

# Entry point from the BUILD file; macro for running each test case's macro and
# declaring a test suite that wraps them together.
def fuchsia_package_resource_test_suite(name, **kwargs):
    # Call all test functions and wrap their targets in a suite.
    _test_provider_contents()
    _test_empty_dest_failure()
    _test_multiple_resources()

    native.test_suite(
        name = name,
        tests = [
            ":provider_contents_test",
            ":multi_provider_contents_test",
            ":empty_dest_should_fail_test",
        ],
        **kwargs
    )
