# Expectation-based testing

A system enabling a Fuchsia test to encode pass/fail expectations and skipped cases for that test.
This is particularly helpful for conformance-style test suites where the test suite itself is
cleanly separable from the system under test.

With this system enabled, expected-to-fail tests that unexpectedly pass will register as test
failures, and vice versa, which is helpful to prompt changes to test expectations to prevent
backsliding.

There are two ways to specify expectations for a set of tests: per-package and per-component.
Per-package expectations apply to all test components within a package. Per-test expectations apply
to only the test cases in that test component. If both mechanisms are used in the same package the
expectations for a test component are used instead of the expectations for the package.

## Usage for per-test expectations

To use test expectations for a particular component, `fuchsia_test_component_with_expectations`
must be used instead of `fuchsia_test_component`:

```gn
import("//src/lib/testing/expectation/fuchsia_test_component_with_expectations.gni")

fuchsia_test_component_with_expectations("expectation-example-test") {
  expectations = "expectations-example-test-expectations.json5"
  manifest = "meta/expectation-example-test.cml"
}

fuchsia_test_package("expectation-example") {
  test_components = [ ":expectation-example-test" ]
}
```

Test components using this template must specify a path to a manifest file, cm_label is not
supported.  Test components may be placed in a regular `fuchsia_test_package` or a
`fuchsia_test_with_expectations` package (see below).

Each component specified using `fuchsia_test_component_with_expectations` must have its manifest
`include` the expectation client shard (./meta/client.shard.cml).


## Usage for per-package expectations

To use test expectations for all tests within a package, `fuchsia_test_with_expectations_package`
must be used instead of `fuchsia_test_package`:

```gn
import(
    "//src/lib/testing/expectation/fuchsia_test_with_expectations_package.gni")

fuchsia_test_with_expectations_package("expectation-example-package") {
  test_components = [ ":some-integration-test" ]
  expectations = "expectations_file.json5"
}
```

Each component specified in `test_components` must have its manifest `include`
the expectation client shard (./meta/client.shard.cml). This is enforced by the
`fuchsia_test_with_expectations_package` GN template.

See the doc comments in ./fuchsia_test_with_expectations_package.gni for more
information.

Pass/fail/skip expectations are specified via a JSON5 file.
See `./example_expectations.json5` for a simple example.

## Cases Generating Error Logs

Due to limitations (see https://fxbug.dev/65359), the testing framework fails
any test suite in which error logs are generated, even if the test case generating
those logs is expected to fail. There are a couple of options here.

### Option 1: Skip cases that generate error logs

Pros: Simple.
Cons: No signal from skipped tests.

### Option 2: Allow error logs in all tests

Run the tests in a package with:

```
test_specs {
  log_settings = {
    max_severity = "ERROR"
  }
}
```

Pro: Retain a signal from skipped tests.
Con: Lose the signal from error logs.

### Option 3: Use the expectation framework's `expect_{pass|failure}`

In the expectation's file, include expects with one of the following `type`s:

* `expect_pass_with_err_logs`
* `expect_failure_with_err_logs`

When instantiating a test package, pass one of the following flags:

* "RUN_ONLY_CASES_WITH_ERROR_LOGS", in which case only those cases with a `with_err_logs`
  expect type will be run.
* "SKIP_CASES_WITH_ERROR_LOGS", in which case those cases with a `with_err_logs` expect
  type will NOT be run.

By default, all cases will be run.

Pros:
* Option to retain both the signal from skipped tests and from error logs (e.g. with
  two packages).
* Just one expectations file.

Con:
* A bit more complex than Options 1 and 2.
