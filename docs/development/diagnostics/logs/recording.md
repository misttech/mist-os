# Recording Logs

There are a few ways you might see a Fuchsia program generate log messages:

* the LogSink service
* the kernel's debuglog
* a combination of these, routed through `stdout` or `stderr`

## `LogSink`/syslog

Components that want to generate log messages call [`fuchsia.logger/LogSink.Connect`]. The
[`fuchsia.logger.LogSink`] service must be [`use`d in the component manifest][syslog-use-shard].

`Connect` takes a socket, into which the actual log messages are [written] by the syslog library.

If the socket's buffer is full, the [writing thread will drop logs]. Dropped messages on the writer
side are counted and that count is sent in the next successful message, after which the counter is
reset. `log_listener` [prints a warning] when it's aware of dropped messages.

The `LogSink` service must drain all of the sockets it receives quickly enough to prevent messages
being dropped on the writer-side. `LogSink` is responsible for draining those sockets to fill
internal buffers (TODO(https://fxbug.dev/47661): No longer true since https://fxrev.dev/490158).
This can result in high CPU usage in both the writing component and the `LogSink` when logging
heavily.

Different languages use different mechanisms to talk to `LogSink`. See the relevant pages for more
information:

* [C++ logging]
* [Dart logging]
* [Go logging]
* [Rust logging]

## debuglog handles

The kernel allows programs to create debuglog handles from the root resource, each handle allowing
its owner to write messages into the [kernel's shared ring buffer]. Each message has a limit of
`ZX_LOG_RECORD_DATA_MAX` bytes, with content in excess being truncated.

In addition to being [bindable to file descriptors], debuglog handles can be passed to the
[`debuglog_write`] and [`debuglog_read`] syscalls. The read syscall is used to transfer from the
debuglog to the system log.

Components that want to send their standard streams to the debuglog gain access through the
[`fuchsia.boot.WriteOnlyLog`] protocol.

Most logs written to debuglog handles are written through stdio forwarding.

## Standard streams: `stdout` and `stderr`

These concepts are familiar to many from other operating systems but their use in Fuchsia can be
complicated to follow because they are routed differently in different parts of the system.

### Drivers

Drivers log to the `LogSink` sink service, but do so through the use of [`zxlogf`]. This function
provides a wrapper around the syslog library, so that each driver will have its own log message
socket.

In addition, `driver_manager` binds `stdout` and `stderr` to debuglog. This allows `driver_manager`
to output critical information to the debuglog, or to fallback to the debuglog for certain product
configurations where the `LogSink` service is not available.

### Components

By default, [components] only have their `stdout` and `stderr` streams captured
if they have access to a `LogSink` service for forwarding. For more information,
see the ELF runner section on [forwarding stdout and stderr streams].

## Forwarding klog to syslog

The Archivist continually reads from the klog and forwards those messages to the main log. Messages
from the klog can be dropped by the pipeline if they are rolled out of the klog buffer before the
archivist reads them into syslog -- these are not yet tracked.

All kernel log messages are sent to the system log with INFO severity because the debuglog syscalls
lack a way to express the severity of a message.

[`fuchsia.logger/LogSink.Connect`]: https://fuchsia.dev/reference/fidl/fuchsia.logger#Connect
[`fuchsia.logger.LogSink`]: https://fuchsia.dev/reference/fidl/fuchsia.logger#LogSink
[syslog-use-shard]: /sdk/lib/syslog/use.shard.cml
[written]: /zircon/system/ulib/syslog/fx_logger.cc?l=72&drc=1bdbf8a4e6f758c3b1782dee352071cc592ca3ab
[writing thread will drop logs]: /zircon/system/ulib/syslog/fx_logger.cc?l=130&drc=1bdbf8a4e6f758c3b1782dee352071cc592ca3ab
[prints a warning]: /src/diagnostics/log_listener/src/main.rs?l=918&drc=3a02d1922c0519b4c7d639879ec0503de9c79f0c
[C++ logging]: /docs/development/languages/c-cpp/logging.md
[Dart logging]: /docs/development/languages/dart/logging.md
[Go logging]: /docs/development/languages/go/logging.md
[Rust logging]: /docs/development/languages/rust/logging.md
[kernel's shared ring buffer]: /zircon/kernel/lib/debuglog/debuglog.cc?l=37&drc=1bdbf8a4e6f758c3b1782dee352071cc592ca3ab
[bindable to file descriptors]: /sdk/lib/fdio/include/lib/fdio/fdio.h?l=36&drc=1bdbf8a4e6f758c3b1782dee352071cc592ca3ab
[`debuglog_write`]: /docs/reference/syscalls/debuglog_write.md
[`debuglog_read`]: /docs/reference/syscalls/debuglog_read.md
[`zxlogf`]: /sdk/lib/driver/compat/cpp/logging.h?l=59&drc=7cf6769e0971693fc1307c6059826596452c75b2
[kernel params]: /docs/reference/kernel/kernel_cmdline.md#drivernamelogflags
[`fuchsia.sys/LaunchInfo`]: https://fuchsia.dev/reference/fidl/fuchsia.sys#LaunchInfo
[`stdout-to-debuglog`]: /src/sys/lib/stdout-to-debuglog
[`fuchsia.boot.WriteOnlyLog`]: https://fuchsia.dev/reference/fidl/fuchsia.boot#WriteOnlyLog
[`ddk/debug.h`]: /src/lib/ddk/include/ddk/debug.h
[components]: /docs/concepts/components/v2/introduction.md
[ELF]: /docs/concepts/components/v2/elf_runner.md
[forwarding stdout and stderr streams]: /docs/concepts/components/v2/elf_runner.md#forwarding_stdout_and_stderr_streams
