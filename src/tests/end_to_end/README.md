# End to end product tests

End to end product tests for fuchsia products are located here.

Unlike other kinds of tests, end to end product tests are specific to a product,
and therefore must not be included in the global `tests` build target mandated
by the [source tree layout](../../docs/development/source_code/layout.md).
Instead, end to end product tests are included directly in the build
configuration of their products in [//products](../../products/).

# Adding your end-to-end test directory

Please also add an `OWNERS` file so that you can edit your test at will. If
there are run-time dependencies needed for the test you can add them to the
`/bundles:end_to_end_deps` group.

# Running an end-to-end test locally

Because end to end tests are tied to a specific product or even architecture,
you should review the test's README.md to see which product or special
instructions are needed to run the test.

To run an end to end test use the `run-e2e-tests` command:

```
$ fx set <product>.<arch> && fx build
$ fx run-e2e-tests name_of_the_test
```

Where `name_of_the_test` is the name of the dart_test rule (like `sl4f_test`).

# Running an end-to-end test in infra
* Refer to [CQ VS CI vs FYI](#CQ-VS-CI-vs-FYI) section to decide which test case
build group the new test belongs to
* Once decided, you can add it accordingly to that group by updating
[Honeydew Infra Test Groups]
* If the desired group is not present in [Lacewing User Tests Infra Groups] then
  * create new group test group in [Lacewing User Tests Infra Groups]
  * Update the corresponding Lacewing builder configuration file (maintained by
    Foundation Infra team) to include this newly created group in
    [Lacewing User Tests Infra Groups]. If your test depends on target side
    packages, make sure to include the appropriate `*_packages` group.
* Update the test case's `python_mobly_test` rule in BUILD.gn to include
  appropriate BOARDS (based on what all the boards this test need to be run in
  infra) in `environments` field ([example](../../testing/end_to_end/examples/test_soft_reboot/BUILD.gn))
* Add yourself to `lacewing-builder-users@google.com` group so that you will be
  notified whenever any of the tests in the builder is failed. You can then
  check for your test result in that run and triage if it has failed.

Please reach out to Lacewing team if you need help with this one.

## CQ VS CI vs FYI
Use the following approach in deciding whether to run the test case in CQ/CI/FYI:
* Any test case that can be run using Emulator will be first run in FYI.
  After 200 consecutive successful runs, test case will be promoted to CQ.
* Any test case that can be run using hardware (NUC, VIM3 etc) will be run in
  FYI and will remain in FYI. (We are exploring options on gradually promoting
  these tests from FYI to CI but at the moment these tests will remain in FYI).

Based on this we have created the following:
* Test case build groups:
  * Test group naming scheme: `<PRODUCT>_<BOARD>_<STABILITY>[ |_sl4f]_tests`
    * `<PRODUCT>` - The product that the tests require to run on - e.g. "core",
        "workbench".
    * `<BOARD>` - The board that the tests require to run on - e.g. "emulator",
        "nuc", "vim3".
    * `<STABILITY>` - Whether tests are stable or flaky - "stable" or "unstable".
        All newly added tests must be added to the "unstable" groups until 200
        passing runs in infra FYI builder have been observed, after which they
        may be promoted to "stable" groups.
    * `[ |_sl4f]` - If tests require SL4F server then include "_sl4f".
        Otherwise, leave it empty.
    * Examples:
      * `core_emulator_stable_tests`
      * `workbench_vim3_unstable_tests`
  * This naming scheme is chosen to facilitate infra builder configuration.
    * `<STABILITY>` informs whether a group is potential to run in CI/CQ
        (as it depends on `<BOARD>` also).
    * `[ |_sl4f]` informs whether a test group can be run on certain products.
    * `<BOARD>` informs whether a test group can be run on certain boards.
* Builder examples:
  * `core.x64-debug-pye2e` - CQ builder to run stable emulator and NUC tests
  * `core.x64-debug-pye2e-users-staging` - FYI builder to run unstable emulator and NUC tests
  * `core.vim3-debug-pye2e-users-staging` - FYI builder to run unstable VIM3 tests
  * format: `<PRODUCT>.<BOARD>-debug-pye2e[-users| ]-[ |staging|ci]`, where
    * for "staging" builders `-users` will be used in the builder name
    * if builder is for "CQ" then no postfix is needed but for other stages,
      postfix is necessary (`-staging` or `-ci`)


[Lacewing User Tests Infra Groups]: BUILD.gn
