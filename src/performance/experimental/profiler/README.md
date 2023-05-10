# CPU Profiler

This is an experimental cpu profiler aimed at sampling stack traces and presenting them as pprof
graphs. It does not have kernel assisted sample yet, though that is an eventual goal after which it
will move out of experimental.

In the mean time, it uses the root resource to suspend the target threads periodically,
`zx_process_read_memory` and the fuchsia unwinder to read stack traces from the target, then
exfiltrates them.

Due to this, there is some overhead when sampling stacks. About 300us per sample when using frame
pointers, 3000us for reading Dwarf CFI frames[^1]. This can make the current implementation
impractical for use cases require demand less perturbation.

[^1]: Numbers measured on core.x64-qemu

## Usage:

You'll need to add the profiler component and its core shard to the build as well as the host tool.

```
fx set <product>.<board> --with-base //src/performance/experimental/profiler --with-host //src/performance/experimental/profiler/samples_to_pprof:install --args='core_realm_shards += [ "//src/performance/experimental/profiler:profiler_core_shard" ]'
```

The easiest way to interact with the profiler is through the ffx plugin:

```
ffx profiler start --pid <target pid> --duration 5
```

This will place a `profile.out` file in your current directory. You'll need to symbolize it and
export it to pprof format.

```
ffx debug symbolize < profile.out > profile.out.sym; fx samples_to_pprof profile.out.sym
```

This will output the file `profile.out.sym.pb` which can be handed to pprof.

```
$ pprof -top profile.out.sym.pb
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
