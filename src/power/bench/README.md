# Power Framework Benchmarks

Power Framework currently has microbenchmarks exercising the TakeWakeLease call
from the System Activity Governor (SAG), and the Lease operation fidls from the
Topology Test Daemon. These benchmarks are based on the
[Criterion](https://docs.rs/criterion/latest/criterion/) benchmark
infrastructure. Benchmark functions and fidl proxy obtaining functions are
separately declared in each some_work.rs and can be run either as standard
integration tests or to profile the performance of the corresponding fidl.
The integration tests run in CQ and can verify the correctness of the
implementations of the same function for fidl proxy creation and benchmarks.

## Running the Benchmarks

1. Add the benchmark test target to your `fx set` line, and configure your
   build for release (optimized). Note that the benchmarks rely on `SL4F`
   which is currently only available on terminal and workstation builds:

    ```
    fx set terminal.x64 --with //src/tests/end_to_end/perf:test --release
      --with-test //src/power:tests #integration test will be included
    ```

2. Build Fuchsia

    ```
    fx build
    ```

3. Start the Fuchsia emulator

    ```
    ffx emu start --headless --net tap
    ```

4. In a separate terminal, serve Fuchsia packages

    ```
    fx serve -v
    ```

5. Run the tests

    ```
    fx test --e2e power_framework_microbenchmarks -o
    ```

6. Run integration tests (optional)

    ```
    fx test -o power-framework-bench-integration-tests --test-filter=*test_topologytestdaemon_toggle -- --repeat 2000
    ```

After completing, the tests will print the name of the
[catapult_json](https://github.com/catapult-project/catapult/blob/main/docs/histogram-set-json-format.md)
output file containing the benchmark results.
