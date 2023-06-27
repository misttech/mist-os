# Debugging a process, component, or crash dump

## Attaching to a process by filters

Running a process on Fuchsia is more complicated than in other systems because there isn't a default
environment (see "A note about launcher environments" below).

The only way to reliably debug all types of processes is to create a filter via "attach" and start
it the normal way you would start that process outside of the debugger.
The debugger supports the following filter criteria.

  * Component moniker (v2 components).
  * Component URL (v2 components).
  * Process name, exact match or partial match.

The "attach" command can interpret the input differently according to the format. For example,
`attach /core/feedback` will be interpreted as a [component moniker](../../reference/components/moniker.md)
and thus creates a filter that matches *all* processes in that component, while `attach fuchsia-boot:///#meta/archivist.cm`
will be interpreted as a [component URL](../../reference/components/url.md) and could possibly
match processes in multiple components.

For convenience, it's allowed to only pass the last part of a component URL to the "attach" command.
So `attach archivist.cm` will function the same as `attach fuchsia-boot:///#meta/archivist.cm`.

If the input doesn't look like some component, it'll be interpreted as a process name.
The launcher of a process could set the process name freely. To check what this is, use "ps"
(either in the debugger or from a system shell) with it running.

> Note: only the first 31 bytes of the name are included in the Zircon process description.

This example sets a pending breakpoint on `main` to stop at the beginning of execution, and waits
for processes in "my\_app.cm" component to start:

```
[zxdb] attach my_app.cm
Waiting for process matching "my_app.cm".

[zxdb] break main
Created Breakpoint 1 @ main
Pending: No current matches for location. It will be matched against new
         processes and shared libraries.
```

Then run the process the way you would in normal use (directly on the command
line, via `fx test`, `ffx component run /core/... fuchsia-pkg://...`, or
another way. The debugger should then immediately break on `main` (it may take
some time to load symbols so you may see a delay before showing the source
code):

```none {:.devsite-disable-click-to-copy}
Attached Process 1 [Running] koid=51590 my_app.cm
🛑 on bp 1 main(…) • main.cc:222
   220 }
   221
 ▶ 222 int main(int argc, const char* argv[]) {
   223   foo::CommandLineOptions options;
   224   cmdline::Status status = ParseCommandLine(argc, argv, &options);
```

You can then do basic commands that are similar to GDB:

```none {:.devsite-disable-click-to-copy}
next
step
print argv[1]
continue
quit
```

## Directly launching in debugger

It's possible to launch an executable, a component, or a test in the debugger directly.

### Launching an executable

Use `run <executable>` to launch an executable from the debugger. Note that in Fuchsia, all
processes need to be run in a namespace. `run` will launch the process in debug_agent's namespace,
which means only executables in the debug_agent package and executables from [bootfs](../../glossary#bootfs)
are available. Since they are run in debug_agent's namespace, the processes will also share the same
capabilities of the debug_agent.

Due to the limitation above, `run` is only meant to be used to run some demo programs.

### Launching a component

Use `run-component <component url>` to launch a component. V2 components will be created in the
[`ffx-laboratory`](../../development/components/run.md#ffx-laboratory) collection, similar to the
behavior of `ffx component run --recreate`. The output of the component will NOT be redirected.
Since all the limitations from `ffx-laboratory` are applied, `run-component` is usually only used
to launch demo programs.

### Launching a test

Use `run-test <test url>` to launch a test, similar to `ffx test run`. Optional case filters can be
provided to specify test cases to run.

Since Fuchsia test runners start one process for each test case, there could be many processes
started in the debugger. These processes will have their names replaced as the names of the test
cases, so that it's easier to navigate between test cases. The output of these processes will be
redirected in the debugger and can be replayed by `stdout` or `stderr`.

## Attaching to an existing process

You can attach to most running processes given the process’ koid (the [kernel object
ID](/docs/concepts/kernel/concepts.md) that, when applied to a process, is equivalent to a process
ID on other systems). You can get the koid by running `ps` on the target Fuchsia system or use
zxdb's built-in `ps` command:

```none {:.devsite-disable-click-to-copy}
[zxdb] ps
j: 1030 root
  j: 1079 zircon-drivers
    p: 1926 driver_host
...
```

In this listing, "j:" indicates a [job](/docs/concepts/kernel/concepts.md) (a container for
processes) and "p:" indicates a process. The number following the type prefix is the object's koid.

Then to attach:

```none {:.devsite-disable-click-to-copy}
[zxdb] attach 3517
Process 1 Running koid=1249 pwrbtn-monitor
```

When you’re done, you can choose to `detach` (keep running) or `kill` (terminate) the process.

## Attaching to processes in specific jobs

It's possible to limit the scope of a filter to specific jobs. In this case, the filter can be empty
which means attaching to all applied processes. For example,

```none {:.devsite-disable-click-to-copy}
[zxdb] ps
 j: 1033 root
   j: 2057
     p: 2274 foobar.cm
     j: 2301
       p: 2307 foo
       p: 2318 bar
```

`attach -j 2301 foo` will only attach to process 2307. `attach -j 2057` will attach to all 3
processes.

## Debugging drivers

It's not currently possible to set up the debugger early enough in system startup to debug most
driver initialization. And since zxdb itself uses the network and filesystem, no drivers associated
with network communication can be debuged, and neither can many drivers associated with storage or
other critical system functions.

You can debug running drivers by attaching to the driver hosts, which can be listed by
`ffx driver list-hosts`. Initialization of the driver can be delayed by calling `WaitForDebugger()`,
given the driver is not depended by the debugger. In C++, use

```cpp
// Add //src/lib/debug to the dependency.
#include "src/lib/debug/debug.h"

debug::WaitForDebugger();
```

or in Rust:

```rust
// Add //src/lib/debug/rust to the dependency.
debug::wait_for_debugger(60);
```

Once the debugger is attached, `WaitForDebugger()` will trigger a software breakpoint.
After necessary setup is performed, type `continue` to continue the execution.

## Debugging crash dumps

Zxdb supports loading a minidump generated by a crash report. Use the "opendump" verb and supply the
local file name of the dump. The debugger must not be attached to another dump or a running system
(use "disconnect" first if so).

```none {:.devsite-disable-click-to-copy}
[zxdb] opendump upload_file_minidump-e71256ba30163a0.dmp
Opening dump file
Dump loaded successfully.
```

Now the thread, stack, and memory commands can be used to inspect the state of the program. Use
`disconnect` to close the dump.

It's also possible to use the [`ffx debug core`](https://fuchsia.dev/reference/tools/sdk/ffx#core)
command, for example:

```none {:.devsite-disable-click-to-copy}
ffx debug core upload_file_minidump-e71256ba30163a0.dmp
```

#### Downloading symbols

Zxdb will automatically search servers registered in the [symbol-index][ffx-debug-symbol-index] for
symbols that are not found in any locally configured symbol directories. You can see the complete
list of symbol servers registered in your symbol-index with the following command:

```posix-terminal
ffx debug symbol-index list -a
```

This will recursively traverse and resolve all symbol-index files that are included in the global
symbol-index. For more information see [About symbol settings][about-symbol-settings]

The servers that appear will be used to download symbol files and load them to the debugger
asynchronously. For particularly large debuginfo files, you may see output like this from `sym-stat`
in zxdb:

```none {:.devsite-disable-click-to-copy}
[zxdb] sym-stat
...
    Base: 0xc37e9000
    Build ID: ba17ef8c43bccf5fb6bf84f9a8d83d9cf3be8976 (Downloading...)
    Symbols loaded: No
...
```

Which indicates that a download is in progress. Once the file is downloaded, zxdb will index the
symbols from the newly downloaded file and debugging can continue.

### Debugging multiple processes

You can debug many arbitrary processes at the same time. Attaching or running when a process is
already running in the debugger will just create a new one in parallel.

Recall from the “Interaction model” section you can list the current processes with:

```none {:.devsite-disable-click-to-copy}
[zxdb] process
  # State       Koid Name
▶ 1 Running     1249 pwrbtn-monitor
  2 Not running 7235 pwrbtn-monitor
```

Select one of those as the default by providing its index (not koid):

```none {:.devsite-disable-click-to-copy}
[zxdb] process 2
```

Or apply commands to a specific process (even if it’s not the default) with:

```none {:.devsite-disable-click-to-copy}
[zxdb] process 2 pause
```

<!-- Reference links -->

[ffx-debug-symbol-index]: ../sdk/ffx/register-debug-symbols.md
[about-symbol-settings]: symbols.md#about_symbol_settings
