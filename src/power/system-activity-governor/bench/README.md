# SystemActivityGovernor (SAG) Benchmarks

SAG currently has microbenchmarks exercising the TakeWakeLease call,
but in the future this may be expanded to include both the Bindings and the
public API surface. SAG benchmarks are based on the
[Criterion](https://docs.rs/criterion/latest/criterion/) benchmark
infrastructure. Benchmark functions are declared in work.rs and can be run
either as standard integration tests or to profile the performance of the
SAG TakeWakeLease fidl. Power Broker and SAG instances are instantiated for the
test in a hermetic environment and the benchmark reuses the same SAG channel.

## Running the Benchmarks

1. Add the benchmark test target to your `fx set` line, and configure your
   build for release (optimized). Note that the benchmarks rely on `SL4F`
   which is currently only available on terminal and workstation builds:

    ```
    fx set terminal.x64 --with //src/tests/end_to_end/perf:test --release
      --with-test //src/power/system-activity-governor:tests #integration test
    ```

2. Build Fuchsia

    ```
    fx build
    ```

3. Start the Fuchsia emulator

    ```
    ffx emu start --headless
    ```

4. In a separate terminal, serve Fuchsia packages

    ```
    fx serve -v
    ```

5. Run the tests

    ```
    fx test --e2e takewakelease_benchmarks -o
    ```

After completing, the tests will print the name of the
[catapult_json](https://github.com/catapult-project/catapult/blob/main/docs/histogram-set-json-format.md)
output file containing the benchmark results.
