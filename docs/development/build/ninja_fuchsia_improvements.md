# Fuchsia-specific Ninja improvements

The Fuchsia build system uses a custom Ninja binary that provides several
improvements to the developer experience. This page describes them.

## Motivation

The motivation for customizing Ninja for Fuchsia is detailed in
[RFC-0153][rfc-0153].

In a nutshell, there are a number of features that would benefit Fuchsia
developers significantly that are hard to get in the upstream version.

All Fuchsia-specific changes are performed on the local `fuchsia-rfc-0153`
branch of our [local Ninja git mirror][fuchsia-mirror]{:.external}, and
are rebased periodically to make them easy to send as GitHub pull requests
to the upstream project, as described in the
[Strategy section of the RFC][rfc-strategy].

## Feature: Status of running commands

Set `NINJA_STATUS_MAX_COMMANDS` in your environment to a strictly positive
integer to let Ninja print, when run in a smart terminal, a table of the
longest running commands, and their elapsed times, just under the status line.
For example, with `export NINJA_STATUS_MAX_COMMANDS=4`, the status could look
like:

```none  {:.devsite-disable-click-to-copy}
[0/28477](260) STAMP host_x64/obj/tools/configc/configc_sdk_meta_generated_file.stamp
  0.4s | STAMP obj/sdk/zircon_sysroot_meta_verify.stamp
  0.4s | CXX obj/BUILD_DIR/fidling/gen/sdk/fidl/fuchsia.me...chsia.media/cpp/fuchsia.media_cpp_common.common_types.cc.o
  0.4s | CXX obj/BUILD_DIR/fidling/gen/sdk/fidl/fuchsia.me...fuchsia.media/cpp/fuchsia.media_cpp.natural_messaging.cc.o
  0.4s | CXX obj/BUILD_DIR/fidling/gen/sdk/fidl/fuchsia.me...dia/cpp/fuchsia.media_cpp_natural_types.natural_types.cc.o
```

The following animated image shows how this looks in practice:

![Ninja multi-line status example](/docs/images/build/ninja-multiline-status.gif)

Note that:

- This feature is automatically disabled in dry-run or verbose invocations
  of Ninja (that is, using the `-n` or `--verbose` flags).

- This feature is automatically disabled when Ninja is not running in an
  interactive / smart terminal.

- This feature is suspended when running console commands as well (visible
  in the example above when running Bazel actions).

- This feature makes it easy to visualize bottlenecks in the build, that is,
  long-lasting commands that prevent other commands to run in parallel.

The commands table updates 10 times per second by default, which is very useful
to understand which long commands are hobbling the build. It is possible to
change the refresh period by setting `NINJA_STATUS_REFRESH_MILLIS` to a decimal
value in milliseconds (not that anything lower than 100 will be ignored since
elapsed times are only printed up to a single decimal fractional digit).

## Feature: GNU Make Jobserver support

The [GNU Make Jobserver protocol][gnu-jobserver] allows a build system to limit
the total number of concurrent jobs (i.e. threads or processes) at any point in
time, even in the presence of recursive build tool invocations.

It requires a top-level **server** to setup a pool of **job slots** that are
shared by participating clients (e.g. compiler, linkers, or even build tools).

The Fuchsia-specific Ninja binary can act both as a client or a server for the
protocol.

You can enable server mode through the `--jobserver` command-line flag, or
setting `NINJA_JOBSERVER=1` in the environment, when starting Ninja.

When Ninja starts, client mode is enabled automatically by looking at the
value of the `MAKEFLAGS` environment variable. This is useful when Ninja is
called from a different build that acts as a server.

In the Fuchsia build, setting `enable_jobserver = true` in `args.gn` allows the
top-level Ninja invocation to start in server mode.

As an example, it is set for the core IDK and core SDK builder configurations,
saving between 6 and 12 minutes of build time. Because these require launching
24+ Ninja sub-builds from the top-level build, which can leverage the protocol
to better coordinate how they each spawn multiple parallel commands.

## Feature: Persistent mode for faster startup times

Note: This feature is currently experimental. We welcome any feedback!

Speed up successive Ninja invocations by setting `NINJA_PERSISTENT_MODE=1` in
your environment. This feature makes Ninja launch a background server process
to read the build manifest once, then keep the build graph in memory between
successive builds.

Note that:

- This feature should be completely transparent, and should not affect
  Ninja's behavior otherwise. It you spot an issue or difference, please
  let us know at `fuchsia-build-team@google.com`!

- Any change in the input `.ninja` file is automatically detected. In this
  case the existing server will be shutdown, and a new one will be started
  automatically. No additional user interaction is required after changing
  a GN build file or performing a `jiri update`.

- The server process will shutdown gracefully after idling for 5 minutes.
  Set `NINJA_PERSISTENT_TIMEOUT_SECONDS=<count>` in your environment
  to change this delay.

- Use `fx build -t server status` to retrieve the status of the server
  for the current build directory.

- Use `fx build -t server stop` to stop any running instance of the server
  explicitly.

- Set `NINJA_PERSISTENT_LOG_FILE=<path>` to send logs related to the
  persistent mode to a given file path.

- Each server process currently takes about 1 GiB of RAM for a `core.x64`
  build configuration. Exact figures will depend on the size of the Ninja
  graph, which depends on your `args.gn` configuration.

- Each Ninja build directory can be served by at most one server process.
  But it is possible to have several processes when using several build
  directories.

- Ninja tools (e.g. `ninja -C <dir> -t commands <target>`) do not run on
  the server yet, so will still use slow startup. This will be fixed in the
  future to speed up queries.

Known bugs / caveats, that will be worked out:

- Mixing persistent and non-persistent builds on the same directory can
  confuse the server at the moment, because independent changes to the Ninja
  build and deps logs are not properly detected. This will be fixed.

  Work-around: use `-t server stop` to stop the server before unsetting
  `NINJA_PERSISTENT_MODE` in your environment.

- "Fast startup" takes a few seconds. For now, every incremental build
  still needs to call stat() for all the files in the build graph, which
  currently takes a few seconds. This will be fixed in the future by
  using inotify / kqueue based filesystem watching features of the host
  operating system, to start immediately when only a few files have been
  modified instead.

- Does not work on Windows (yet). This is due to a technical Win32 limitation
  that prevents copying console handles to other processes. This is mostly an
  issue for the upstream Ninja team, since Fuchsia development does not
  happen on Windows.

[rfc-0153]: /docs/contribute/governance/rfcs/0153_ninja_customization.md
[rfc-strategy]: /docs/contribute/governance/rfcs/0153_ninja_customization.md#branch-strategy
[fuchsia-mirror]: https://fuchsia.googlesource.com/third_party/github.com/ninja-build/ninja/
[gnu-jobserver]: https://www.gnu.org/software/make/manual/html_node/Job-Slots.html
