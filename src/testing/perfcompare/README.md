# perfcompare: Performance comparison tool

## Dependencies

The perfcompare tool requires Python 3.

There are two ways to run the perfcompare tool with the required
dependencies (currently just the `scipy` Python library):

* Via `vpython3`:

  ```sh
  ./prebuilt/third_party/vpython/vpython3 src/testing/perfcompare/perfcompare.py
  ```

  This will automatically download prebuilt, hermetic versions of
  dependencies.  `vpython3` is used for running perfcompare on the
  Infra builders.

* On Linux, when using Debian/Ubuntu, the dependencies can be
  installed using APT:

  ```sh
  sudo apt-get install python3-scipy
  ```

## Running the unit tests

The unit tests used to be run on CI/CQ, but that regressed.  Running
them on CI/CQ is somewhat difficult now because of the dependency on
`scipy`, so for now the tests must be run manually, with:

```sh
python3 src/testing/perfcompare/perfcompare_test.py
```

## Example: Running perf tests locally and comparing results

The perfcompare tool can be used to run perf tests locally and to
compare the results.

As an example, suppose you want to compare the results from
`rust_inspect_benchmarks_test` on two Git commits,
`BEFORE_VERSION` and `AFTER_VERSION`.

This test case is tested in the `terminal` product and may not work in
a different product.

The following commands would gather a dataset of perf test results for
`BEFORE_VERSION` and save them in the directory `perf_results_before`:

```sh
git checkout BEFORE_VERSION
# Covers dependencies of rust_inspect_benchmarks_test.
fx set terminal.x64 --with //bundles/buildbot/terminal
fx build
fx update
python src/testing/perfcompare/perfcompare.py run_local \
  --boots=5 \
  --iter_cmd='fx test --e2e rust_inspect_benchmarks_test' \
  --iter_file='out/test_out/*/*.fuchsiaperf.json' \
  --dest=perf_results_before
```

These commands would do the same, but for `AFTER_VERSION`, saving the
results dataset in a different directory, `perf_results_after`:

```sh
git checkout AFTER_VERSION
fx build
fx update
python3 src/testing/perfcompare/perfcompare.py run_local \
  --boots=5 \
  --iter_cmd='fx test --e2e rust_inspect_benchmarks_test' \
  --iter_file='out/test_out/*/*.fuchsiaperf.json' \
  --dest=perf_results_after
```

Note that the `run_local` commands will reboot Fuchsia.

The two datasets can then be compared with the following command,
which prints a table showing the "before" and "after" results side by
side:

```sh
python3 src/testing/perfcompare/perfcompare.py compare_perf perf_results_before perf_results_after
```
