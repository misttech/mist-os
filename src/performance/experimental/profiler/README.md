# CPU Profiler

This is an experimental cpu profiler aimed at sampling stack traces and outputting them to pprof
format.

## Usage:

See [Profiling Cpu Usage](//docs/development/profiling/profiling-cpu-usage.md) for a how to profile
cpu usage using this profiler.

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

