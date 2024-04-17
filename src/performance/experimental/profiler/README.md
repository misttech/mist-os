# CPU Profiler

This is an experimental cpu profiler aimed at sampling stack traces and outputting them to pprof
format.

## Kernel assistance

Experimental kernel assisted sampling via `zx_sampler_create` can be enabled via the GN flag
'experimental_thread_sampler_enabled=true' which improves sampling times to single digit us per
sample.

If not enabled, the sampler will fall back to userspace based sampling which uses the root resource
to suspend the target threads periodically, uses `zx_process_read_memory` and the fuchsia unwinder to
read stack traces from the target, then exfiltrates the stack data. In this mode taking a sample
using frame pointers takes roughly 300us[^1] per sample. In this fallback mode, it's recommended to
set a relatively low sample rate to not overly perturb the profiled program. For reference, 50
samples per second would be a 1.5% sampling overhead.

Note: As experimental_thread_sampler_enabled=true isn't enabled in CI/CQ yet, integration tests
need to be run locally with the build flag enabled -- CI/CQ results will reflect the state of the
zx_process_read_memory based implementation.

[^1]: Numbers measured on core.x64-qemu

## Usage:

You'll need to add the host tool to interpret the output file to your build. In addition, we
recommend several additional arguments to improve the quality of the profiles you get:

```
fx set <product>.<board> \
--with-host //src/performance/experimental/profiler/samples_to_pprof:install \
--args='debuginfo="backtrace"' \
--args='enable_frame_pointers=true' \
--args='experimental_thread_sampler_enabled=true'
```

'debuginfo="backtrace"' and 'enabled_frame_pointers=true' will give enough information to retrieve
and symbolize stacks in a release build.


### Release Mode
`--release` may result in more difficult to follow stacks due to inlining and optimization passes, but
will give overall more representative samples. Especially for Rust and C++ based targets, the zero
cost abstractions in their standard libraries are not zero cost unless some level of optimization is
applied.

# Profiling
The easiest way to interact with the profiler is through the ffx plugin:

```
ffx profiler start --pid <target pid> --duration 5
```

This will place a `profile.pb` file in your current directory which can be handed to pprof.

```
$ pprof -top profile.out.pb
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
