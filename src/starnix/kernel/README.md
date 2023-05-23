# Starnix

Starnix runs unmodified Linux binaries on Fuchsia. We have tested Starnix with
glibc and bionic based binaries. Patches that improve Linux compatibility are
welcome!

## How to run starnix

### Configure your build

In order to run starnix, we need to build `//src/starnix`:

```sh
fx set core.x64 --with //src/starnix,//src/starnix:tests
fx build
```

> Note: If you have `//vendor/google` in your source tree, you might want to add
> `//vendor/google/starnix:tests` to the `fx set` command above to include some
> additional, non-public tests.

### Run Fuchsia

Run Fuchsia as normal, for example using `fx serve` and `ffx emu start --headless`.

To monitor starnix, look for log messages with the `starnix` tag:

```sh
ffx log --filter starnix --severity TRACE --select "core/starnix_runner/kernels*#TRACE"
```

When running tests, you will need to pass the log selection parameters to fx test instead:

```sh
fx test --min-severity-logs TRACE <test name>
```


The `--select` arguments contain the moniker for the starnix instance whose minimum severity log
level you want to change. This affects the logs emitted by starnix, as opposed to `--severity`,
which affects which logs are filtered for viewing. The changed log level only persists for the
duration of the `ffx log` command.

If you do not care about detailed logging, you can leave out the `--severity` and just do:

```sh
ffx log --filter starnix
```

Starnix produces a large amount of logs and this can overload archivist's ability to
retain them, instead printing messages like:

```text
[starnix] WARNING: Dropped logs count: 5165
```

If you see this, you can reduce or eliminate the dropped messages by increasing
the value of `max_cached_original_bytes` the the JSON file
`//src/diagnostics/archivist/configs/archivist_config.json' in your local checkout.
Increasing by a facfor of 10 seems to work well.

### Run a Linux binary

Running a Linux binary manually involves two steps. First, you need to start the
container in which the binary will run. This step is analogous to booting a
virtual machine:

```sh
ffx component run /core/starnix_runner/playground:<container-name> <container-url>
```

In this command, you pick a `<container-name>` that you can use to refer to
this container later. You can run as many instances of a container as you wish
as long as you given them different names.

The `<container-url>` is the component URL for the container you wish to run.
For example, `fuchsia-pkg://fuchsia.com/starless#meta/empty_container.cm` is
the component URL for the empty container, which does not have a `libc.so`
and, therefore, cannot run dynamicly linked C binaries.

Once the container is running, you can run Linux binaries inside that
container using the component URL for that binary:

```sh
ffx component run --connect-stdio \
    /core/starnix_runner/playground:<container-name>/daemons:<component-name> \
    <component-url>
```

The `--connect-stdio` flag is optional, but specifying this flag will cause
stdio, stdout, and stderr from your terminal to be connected to the binary.
Notice that this command re-uses the `<container-name>` you picked for the
previous command. This name indicates the container in which the process
will run.

Similar to the previous command, you pick a `<component-name>` for the
component that represents this process. When the process exits, this component
will disappear from the component topology.

The `<component-url>` is the component URL for the binary you wish to run. For
example, `fuchsia-pkg://fuchsia.com/hello_starnix#meta/hello_starnix.cm` is the
component URL for the `hello_starnix` binary. The component manifest specifies
which binary to run. The binary can be inside the container (e.g., `/bin/sh`)
or the binary can be in the package that contains the component.

To terminate the container, use the `ffx component stop` command.

See [`hello_starnix`](../examples/hello_starnix/README.md) for how to run a
minimal binary in an empty container.

### Getting a shell

Once you have a Starnix container running, you can attach a console to that
container and run a shell. For example, if you have created a container with
the moniker `/core/starnix_runner/playground:<container-name>`, you can use the
following command to attach a shell:

```sh
ffx starnix console -m /core/starnix_runner/playground:<container-name> /bin/bash
```

This command assumes the container has a shell binary at `/bin/bash`. If you wish
to run another binary, you have to specify the full path.

If you omit the `-m` argument, `ffx starnix console` will look for a Starnix
container in the Fuchsia session.

### Run a Linux test binary

Linux test binaries can also be run using the Starnix test runner using the
standard `fx test` command:

```sh
fx test exit_test --output
```

You should see output like:

```text
[==========] Running 3 tests from 1 test suite.
[----------] Global test environment set-up.
[----------] 3 tests from ExitTest
[ RUN      ] ExitTest.Success
[       OK ] ExitTest.Success (4 ms)
[ RUN      ] ExitTest.Failure
[       OK ] ExitTest.Failure (3 ms)
[ RUN      ] ExitTest.CloseFds
```

If you set the log level to `TRACE` (e.g.,  `ffx log --severity TRACE --select "core/test*/*/starnix*#TRACE"`), you should see the system call handling in the device logs:

```text
[629.603][starnix][D] 1[/data/tests/exit_test] wait4(0x3, 0x1c48095b950, 0x0, 0x0, 0x10, 0x10)
[629.603][starnix][D] 3[/data/tests/exit_test] prctl(0x53564d41, 0x0, 0x700d5ea000, 0x3000, 0x3a506c7a34b, 0xc06913ece9)
[629.603][starnix][D] 3[/data/tests/exit_test] -> 0x0
[629.604][starnix][D] 3[/data/tests/exit_test] exit_group(0x1, 0x3, 0x2b18e3464f8, 0x3000, 0x3a506c7a34b, 0xc06913ece9)
[629.604][starnix][I] exit_group: pid=3 exit_code=1
```

For GUnit tests (such as the syscall tests in //src/starnix/tests/gvisor),
you can run specific tests with the `--test-filter` flag. For example,

```sh
fx test epoll_test --output --test-filter="EpollTest.AllWritable"
```

Specifying `*` as the filter turns on all tests in the binary.

### Making changes to Starnix when using ffx

The Starnix instances that `ffx` connects are static in the component hieararchy. This means that
they need to be stopped explicitly in order to be updated when changes have been made to the Starnix
runner code.

```sh
ffx component stop starnix_kernel
```

If more than one Starnix instance is running, the above command will list the running Starnix instances and you can stop them individually.

Alternatively, use the following command to stop all the instances at once:

```sh
ffx target ssh killall starnix_kernel.cm
```

## Testing

### Writing in-process unit tests

Decorate your test function with the `#[::fuchsia::test]` macro instead of the standard `#[test]`
macro. `#[::fuchsia::test]` will initialize logging so that failing tests can be debugged more
easily.

### Running the in-process unit tests

Starnix also has in-process unit tests that can interact with its internals
during the test. To run those tests, use the following command:

```sh
fx test starnix-tests
```

### Using a locally built syscalls test binary

The `syscalls_test` test runs a prebuilt binary that has been built with the
Android NDK. You can substitute your own prebuilt binary using the
`starnix_syscalls_test_label` GN argument:

```sh
fx set core.x64 --args 'starnix_syscalls_test_label="//local/starnix/syscalls"' --with //src/starnix,//src/starnix:tests
```

Build your `syscalls` binary and put the file in `//local/starnix/syscalls`.
(If you are building using the Google-internal build system, be sure to
specific the `--config=android_x86_64` build flag to build an NDK binary.)

You can then build and run your test as usual:

```sh
fx build
fx test syscalls_test
```

### Viewing Inspect for debugging

You can view Inspect data exposed by starnix using `ffx inspect`.

To view the thread groups currently running, run:

```
ffx inspect show core/starnix_runner/bionic:root/container/kernel/thread_groups
```

You can also view the number of syscalls that have been executed (after enabling
the "syscall_stats" feature):

```
ffx inspect show core/starnix_runner/bionic:root:syscall_stats
```

## Tracing

Starnix is integrated with Fuchsia's tracing system but is disabled by default.
Set the build argument `starnix_disable_tracing` to false to enable tracing. To
start a trace with an increased buffer size, run:

```
ffx trace start --categories "kernel:meta,starnix" --buffer-size 64
```

Trace files can be visualized and queried with Perfetto. For example, to see the average
time spent in starnix during a `clock_getres` syscall, run the query:

```
select avg(dur), count(*)
from slice join args using (arg_set_id)
where key='name' and display_value='clock_getres' and name='RunTaskLoop'
```

[adb.docs]: https://developer.android.com/studio/command-line/adb#copyfiles
