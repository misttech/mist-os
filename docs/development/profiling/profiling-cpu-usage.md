# Profiling CPU Usage

`ffx profiler` is an experimental tool that allows you to find and visualize
hotspots in your code. The CPU profiler periodically samples your running
threads and records backtraces, which can be viewed with the
[`pprof`](https://github.com/google/pprof){:.external} tool.

## Tutorial

To enable and use the CPU profiler in your Fuchsia environment,
do the following:

1. Add some extra arguments to your Fuchsia configuration using the
   `fx set` command, for example:

   ```posix-terminal
   fx set <PRODUCT>.<BOARD> \
   --release \
   --args='enable_frame_pointers=true' \
   --args='experimental_thread_sampler_enabled=true'
   ```

   - `experimental_thread_sampler_enabled=true` enables experimental
     sampling support.
   - `enable_frame_pointers=true` enables the profiler to collect stack
     samples.
   - `debuginfo="backtrace"` adds the needed debug info to symbolize stacks.

   Note: Adding the `--release` flag may result in more difficult to follow
   stacks due to inlining and optimization passes. However, it will give overall
   more representative samples. Especially for Rust and C++ based targets, the
   zero cost abstractions in their standard libraries are not zero cost unless
   some level of optimization is applied.

2. Interact with the CPU profiler using the `ffx profiler` command,
   for example:

   ```posix-terminal
   ffx profiler attach --pid <TARGET_PID> --duration 5
   ```

   This command profiles `<TARGET_PID>` for 5 seconds, then creates
   a `profile.pb` file in your current directory, which can be handed to
   the `pprof` tool.

3. Use `pprof` to export to various format, including text and interactive
   Flame graph, for example:

   ```posix-terminal
   pprof -top profile.pb
   ```

   This command produces output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   Main binary filename not available.
   Type: location
   Showing nodes accounting for 272, 100% of 272 total
         flat  flat%   sum%        cum   cum%
          243 89.34% 89.34%        243 89.34%   count(int)
           17  6.25% 95.59%        157 57.72%   main()
            4  1.47% 97.06%          4  1.47%   collatz(uint64_t*)
            3  1.10% 98.16%          3  1.10%   add(uint64_t*)
            3  1.10% 99.26%          3  1.10%   sub(uint64_t*)
            1  0.37% 99.63%          1  0.37%   rand()
            1  0.37%   100%          1  0.37%  <unknown>
            0     0%   100%        157 57.72%   __libc_start_main(zx_handle_t, int (*)(int, char**, char**))
            0     0%   100%        154 56.62%   _start(zx_handle_t)
            0     0%   100%        160 58.82%   start_main(const start_params*)
   ```

## Start and attach to targets

### Pids, Tids, and Job Ids

The easiest way to to get started is to attach to `koids`:

```posix-terminal
ffx profiler attach --duration 5 --pids 123,1234,234 --tids 345,234 --job-ids 123
```

This command attaches to all of the specified `pids`, `tids`, and `job_ids`.
If `pids` are specified, the profiler also attaches to each thread in the process.
If `job_ids` are specified, the profiler attaches to each process and thread in
the job and also attaches to each child job and each process and thread in the
child jobs.

If you don’t know your `pid`, you can try identifying it with the following
command:

```posix-terminal
ffx target ssh ps | grep fuchsia_microbenchmarks
```

### Components

The profiler can attach to an existing component using its moniker or URL,
for example:

```posix-terminal
ffx profiler attach --moniker core/ffx-laboratory:your_component
```

```posix-terminal
ffx profiler attach --url 'fuchsia-pkg://fuchsia.com/your_component#meta/your_component.cm'
```

The profiler can also launch your component and attach to it as soon as
it is ready, for example:

```posix-terminal
ffx profiler launch --duration 5 --url 'fuchsia-pkg://fuchsia.com/your_component#meta/your_component.cm'
```
