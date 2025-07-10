A cli tool to inspect .fxt files

Usage:

```
fx set ... --with-host //src/performance:fxtdump
# e.g. to locate invalid records in a trace
fx fxtdump dump trace.fxt --strict
...
...
0x0d9ffb40: Event(EventRecord { provider: Some(Provider { id: 61, name: "starnix_kernel.cm" }), timestamp: 935523250156, process: ProcessKoid(46458), thread: ThreadKoid(46460), category: "kmem_stats_a", name: "starnix:pager", args: [Arg { name: "name", value: String("write") }], payload: DurationEnd })
ERROR couldn't parse string as utf-8
```
Will get you the offset into the trace directly before where which we encountered a corrupted
record.
