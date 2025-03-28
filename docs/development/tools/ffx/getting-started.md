# Getting started with ffx

This doc will guide you through some of the features of `ffx`. For an overview
of the design and components of `ffx`, see [the ffx overview](/docs/development/tools/ffx/overview.md).

## Contacting the ffx team

If you discover possible bugs or have questions or suggestions,
[file a bug](https://issues.fuchsia.dev/issues/new?component=1378294&template=1838957).

## Prerequisites

To follow the examples in this doc, you'll need a Fuchsia device running. If you
don't have a physical device connected, you can use an emulator.

To start an emulator with networking enabled but without graphical user
interface support, run `ffx emu start --headless`.

For more information on configuring the emulator
see, [Start the Fuchsia emulator](/docs/get-started/set_up_femu.md).

Your device must be running a `core`
[product configuration](/docs/development/build/build_system/boards_and_products.md)
or a product configuration that extends `core` (such as `workstation_eng`).

Optionally, you can run `ffx log`, which will provide some additional information
about the interactions between `ffx` and your Fuchsia target device.

## Introduction

After following all the prerequisites, run the following in a terminal:

```posix-terminal
ffx help
```

This will list all of the available `ffx` subcommands. You'll see something
like:

```none
Usage: ffx [-c <config>] [-e <env>] [-t <target>] [<command>] [<args>]

Fuchsia's developer tool

Options:
  -c, --config      override default configuration
  -e, --env         override default environment settings
  -t, --target      apply operations across single or multiple targets
  -o, --log-output  specify destination of log output
  --help            display usage information

Commands:
  component         Discover and manage components
  config            View and switch default and user configurations
  daemon            Interact with/control the ffx daemon
  diagnostic        Run diagnostic tests on Fuchsia targets
  docs              View suite of docs for ffx and for Fuchsia
  doctor            Run common checks for the ffx tool and host environment
  emulator          Start and manage Fuchsia emulators
  overnet           Interact with the Overnet mesh
  package           Create and publish Fuchsia packages
  sdk               Modify or query the installed SDKs
  target            Interact with a target device or emulator
  version           Print out ffx tool and daemon versions
```

You can use `ffx help <subcommand>` or `ffx <subcommand> --help` to see
more about any subcommand.

## Interacting with target devices

In a terminal, run the following:

```posix-terminal
ffx target list
```

You'll see a list of devices that `ffx` has discovered. For example, with a
single emulator running, output looks like:

```none
NAME                    SERIAL       TYPE       STATE      ADDRS/IP                       RCS
fuchsia-emulator  <unknown>    Unknown    Product    [fe80::5054:ff:fe63:5e7a%4]    N
```

`RCS`: Indicates whether there is a reachable instance of the Remote Control
Service (RCS) running on the device.

If multiple devices are connected, you must follow the steps in
[Interacting with multiple devices](#interacting-with-multiple-devices) to
specify which target to interact with.

In most cases, if you just interact with a target that should be
sufficient to start a connection. For example:

```posix-terminal
ffx target echo
```

Then the next time you list targets you should see that an `RCS` connection
is active.

```none
$ ffx target list
NAME                    SERIAL       TYPE       STATE      ADDRS/IP                       RCS
fuchsia-emulator  <unknown>    Unknown    Product    [fe80::5054:ff:fe63:5e7a%4]    Y
```

If you had `ffx log` running, you should also see something like the following in
the logs:

```none
[00009.776170][28540][28542][remote-control, remote_control_bin] INFO: published remote control service to overnet
```

## Interacting with multiple devices

When multiple targets are visible in `ffx target list`, you must set a target
as the default or explicitly set the target.

Otherwise, `ffx` commands requiring a target interaction will fail since it's
ambiguous which device to use.

Note: If the default or explicit target has been specified, and you are unable
to run that command against the target, [reach out](#contacting_the_ffx_team)
to the `ffx` team.

### Setting a default target

To set a default target, run:

```posix-terminal
fx set-device $NODENAME
```

You can run the following command to verify that the default target was set
correctly:

```posix-terminal
ffx target default get
```

The target list command shows an asterisk (`*`) next to the name of the
default target. To see the target list:

```posix-terminal
ffx target list
```

### Explicitly specifying a target

To specify which target to use in one-off cases (such as flashing), you can specify
the `-t` or `--target` flag to `ffx` commands, for example:

```posix-terminal
# These 2 commands are equivalent.
ffx --target $NODENAME target flash
ffx -t $NODENAME target flash
```

For `fx` commands, the flag's name is `-d` instead of `-t|--target`. For example:

```posix-terminal
fx -d $NODENAME serve
```

### Controlling the state of target devices

You can use the `target off` and `target reboot` subcommands to power-off or
reboot a device, respectively.

## `ffx` logs

### Destination

Logs normally go to a cache directory (on Linux, usually
`$HOME/.local/share/Fuchsia/ffx/cache/logs`).
The location can be found by running

```posix-terminal
ffx config get log.dir
```

However, the location can be overridden with `-o/--log-output <destination>`,
where `<destination>` can be a filename, or stdout (by specifying `stdout`
or `-`), or stderr (by specifying `stderr`).

### Log Level

The debugging level can be specified with `-l/--log-level <level>`,
where `<level>` is one of `off`, `error`, `warn`, `info`, `debug`, or `trace`.
The default is `info`.

It can also be permanently set by configuring `log.level`, e.g.:

```posix-terminal
ffx config set log.level debug
```

### Interactive Use

A common use of the above options is to see debugging for a specific command:

```posix-terminal
ffx -l debug -o - target echo
```

The above command will produce debugging logs on the command line as part
of the invocation.

#### Target Levels

Specific log "targets" can have a different level, by specifying
configuration entries under `log.target_levels`. For instance, to
see debug logs only for `analytics`:

```posix-terminal
ffx config set log.target_levels.analytics debug
```

Log "targets" are simply prefixes to a log line.

## Configuration

See documentation for the [config command](/docs/development/tools/ffx/commands/config.md).

## Interacting with Components

### Monikers

Many `ffx` commands that use components take monikers as a parameter. You can read more about
monikers and their syntax in [component moniker documentation](/docs/reference/components/moniker.md).

### Finding components

The `component list` command will output monikers of all components that currently exist
in the component topology.

```none
$ ffx component list
/
/bootstrap
/bootstrap/archivist
/bootstrap/base_resolver
/bootstrap/console
/bootstrap/console-launcher
/bootstrap/cr50_agent
/bootstrap/device_name_provider
/bootstrap/driver_index
/bootstrap/driver_manager
/bootstrap/flashmap
/bootstrap/fshost
/bootstrap/fshost/blobfs
/bootstrap/fshost/blobfs/decompressor
...
```

You can use the `component select capability` command to search for components that use/expose
a capability with a given name.

The following command will display all components that use/expose the `diagnostics` capability:

```none
$ ffx component capability diagnostics
Exposed:
  /bootstrap/archivist
  /bootstrap/base_resolver
  /bootstrap/driver_manager
  /bootstrap/fshost
  /bootstrap/fshost/blobfs
  /bootstrap/fshost/blobfs/decompressor
  /bootstrap/fshost/minfs
  /bootstrap/pkg-cache
  /bootstrap/power_manager
  ...
```

### Inspecting a component

You can use the `component show` command to get detailed information about a specific
component.

`component show` allows partial matching on URL, moniker and component instance ID.

The following command will display information about the `/core/network/dhcpd` component:

```none
$ ffx component show dhcpd
               Moniker:  /core/network/dhcpd
                   URL:  #meta/dhcpv4_server.cm
           Instance ID:  20b2c7aba6793929c252d4e933b8a1537f7bfe8e208ad228c50a896a18b2c4b5
                  Type:  CML Component
       Component State:  Resolved
 Incoming Capabilities:  /svc/fuchsia.net.name.Lookup
                         /svc/fuchsia.posix.socket.packet.Provider
                         /svc/fuchsia.posix.socket.Provider
                         /svc/fuchsia.stash.SecureStore
                         /svc/fuchsia.logger.LogSink
  Exposed Capabilities:  fuchsia.net.dhcp.Server
           Merkle root:  521109a2059e15acc93bf77cd20546d106dfb625f2d1a1105bb71a5e5ea6b3ca
       Execution State:  Running
          Start reason:  '/core/network/netcfg' requested capability 'fuchsia.net.dhcp.Server'
         Running since:  2022-09-15 16:07:48.469094140 UTC
                Job ID:  28641
            Process ID:  28690
 Outgoing Capabilities:  fuchsia.net.dhcp.Server
```

### Verifying capability routes

You can use the `component doctor` command to verify that all capabilities
exposed and used by a component are successfully routed.

For example:

```none
$ ffx component doctor /bootstrap/archivist
Querying component manager for /bootstrap/archivist
URL: fuchsia-boot:///#meta/archivist.cm
Instance ID: None

      Used Capability                      Error
 [✓]  fuchsia.boot.ReadOnlyLog             N/A
 [✓]  fuchsia.boot.WriteOnlyLog            N/A
 [✓]  fuchsia.component.DetectBinder       N/A
 [✓]  fuchsia.component.KcounterBinder     N/A
 [✓]  fuchsia.component.PersistenceBinder  N/A
 [✓]  fuchsia.component.SamplerBinder      N/A
 [✓]  fuchsia.sys.internal.ComponentEvent  N/A
      Provider
 [✓]  fuchsia.sys.internal.LogConnector    N/A
 [✓]  config-data                          N/A

      Exposed Capability                   Error
 [✓]  fuchsia.diagnostics.ArchiveAccessor  N/A
      feedback
 [✓]  fuchsia.diagnostics.ArchiveAccessor  N/A
      .legacy_metrics
 [✓]  fuchsia.diagnostics.ArchiveAccessor  N/A
      .lowpan
 [✓]  diagnostics                          N/A
 [✓]  fuchsia.diagnostics.ArchiveAccessor  N/A
 [✓]  fuchsia.diagnostics.LogSettings      N/A
 [✓]  fuchsia.logger.Log                   N/A
 [✓]  fuchsia.logger.LogSink               N/A
```

```none
$ ffx component doctor /core/feedback
Querying component manager for /core/feedback
URL: fuchsia-pkg://fuchsia.com/forensics#meta/feedback.cm
Instance ID: eb345fb7dcaa4260ee0c65bb73ef0ec5341b15a4f603f358d6631c4be6bf7080

      Used Capability                      Error
 [✓]  fuchsia.boot.ReadOnlyLog             N/A
 [✓]  fuchsia.boot.WriteOnlyLog            N/A
 [✓]  fuchsia.diagnostics.FeedbackArchive  N/A
      Accessor
 [✓]  fuchsia.hardware.power.statecontrol  N/A
      .RebootMethodsWatcherRegister
 [✓]  fuchsia.hwinfo.Board                 N/A
 [✓]  fuchsia.hwinfo.Product               N/A
 [✓]  fuchsia.metrics.MetricEventLoggerFa  N/A
      ctory
 [✓]  fuchsia.net.http.Loader              N/A
 [✓]  fuchsia.process.Launcher             N/A
 [✓]  fuchsia.sysinfo.SysInfo              N/A
 [✓]  fuchsia.ui.activity.Provider         N/A
 [✗]  fuchsia.feedback.DeviceIdProvider    `/core/feedback` tried to use `fuchsia.feedback.DeviceIdProvider` from its parent,
                                           but the parent does not offer that capability. Note, use clauses in CML default to
                                           using from parent.
 ...
```

### Running a component

The `component run` command can create and launch components in a given isolated collection.

Note: `fx serve` must be running in order to run a package that is not
[in base or cached](/docs/development/build/build_system/boards_and_products.md#dependency_sets).

Here's an example of running the Rust `hello-world` component in the `/core/ffx-laboratory`
collection. First, you'll need the hello-world package in your universe:

```none
$ fx set <product>.<board> --with //examples/hello_world/rust:hello-world-rust && fx build
...
```

Then use the `component run` command to create and launch a component instance from the URL
`fuchsia-pkg://fuchsia.com/hello-world-rust#meta/hello-world-rust.cm` with the moniker
`/core/ffx-laboratory:hello-world-rust`:

```none
$ ffx component run /core/ffx-laboratory:hello-world-rust fuchsia-pkg://fuchsia.com/hello-world-rust#meta/hello-world-rust.cm
URL: fuchsia-pkg://fuchsia.com/hello-world-rust#meta/hello-world-rust.cm
Moniker: /core/ffx-laboratory:hello-world-rust
Creating component instance...
...
$ ffx component show hello-world-rust
               Moniker: /core/ffx-laboratory:hello-world-rust
                   URL: fuchsia-pkg://fuchsia.com/hello-world-rust#meta/hello-world-rust.cm
                  Type: v2 dynamic component
       Execution State: Running
                Job ID: 50775
            Process ID: 50819
...
```

## Resolving connectivity issues

If you're experiencing problems communicating with a target device using `ffx`,
you can use the `doctor` command to diagnose and attempt to resolve them. If you
file a bug that involves a target device, we'll typically ask for the output
from `ffx doctor` to provide information about where the problem is.

`doctor` will attempt to communicate with the ffx daemon, killing
and restarting it if needed. If this is successful, it will attempt to SSH into
a target device and start the Remote Control Service.

If you try running `ffx doctor` under normal circumstances, you should see:

```none
$ ffx doctor

Doctor summary (to see all details, run ffx doctor -v):

[✓] FFX Environment Context
    [✓] Kind of Environment: Fuchsia.git In-Tree Rooted at /usr/local/google/home/username/fuchsia, with default build directory of /usr/local/google/home/username/fuchsia/out/default
    [✓] Environment-default build directory: /usr/local/google/home/username/fuchsia/out/default
    [✓] Config Lock Files
        [✓] /usr/local/google/home/username/global_ffx_config.json locked by /usr/local/google/home/username/global_ffx_config.json.lock
    [✓] SSH Public/Private keys match

[✓] Checking daemon
    [✓] Daemon found: [3338687]
    [✓] Connecting to daemon

[✓] Searching for targets
    [✓] 1 targets found

[✓] Verifying Targets
    [✓] Target: fuchsia-emulator
        [i] Running `ffx target show` against device

[✓] No issues found
```

If `doctor` fails, it will try to suggest a resolution to the problem. You can [file a bug](https://issues.fuchsia.dev/issues/new?component=1378294&template=1838957) for the ffx team if you
persistently have problems. For example, if `doctor` is unable to start the RCS,
you would see the following:

```none
$ ffx doctor -v

Doctor summary:

[✓] FFX doctor
    [✓] Frontend version: 2025-03-25T18:48:31+00:00
    [✓] abi-revision: 0xB5D2EBDA9DA50585
    [✓] api-level: 26
    [i] Path to ffx: /usr/local/google/home/username/fuchsia/out/default/host_x64/ffx

[✓] FFX Environment Context
    [✓] Kind of Environment: Fuchsia.git In-Tree Rooted at /usr/local/google/home/username/fuchsia, with default build directory of /usr/local/google/home/username/fuchsia/out/default
    [✓] Environment File Location: /usr/local/google/home/username/.local/share/Fuchsia/ffx/config/.ffx_env
    [✓] Environment-default build directory: /usr/local/google/home/username/fuchsia/out/default
    [✓] Config Lock Files
        [✓] /usr/local/google/home/username/global_ffx_config.json locked by /usr/local/google/home/username/global_ffx_config.json.lock
    [✓] SSH Public/Private keys match

[✓] Checking daemon
    [✓] Daemon found: [3338687]
    [✓] Connecting to daemon
    [✓] Daemon version: 2025-03-25T18:48:31+00:00
    [✓] path: /usr/local/google/home/username/fuchsia/out/default/host_x64/ffx
    [✓] abi-revision: 0xB5D2EBDA9DA50585
    [✓] api-level: 26
    [✓] Default target: (none)

[✓] Searching for targets
    [✓] 1 targets found

[✗] Verifying Targets
    [✗] Target: fuchsia-emulator
        [✓] Compatibility state: supported
            [✓] Host overnet is running supported revision
        [✓] Opened target handle
        [✗] Timeout while connecting to RCS

[✗] Doctor found issues in one or more categories.
```

## Testing with ffx

The `ffx` command is useful when writing integration tests which need to interact
with the Fuchsia environment. However, since `ffx` is primarily designed for
developers, it inspects the current environment for configuration and also starts
a daemon in the background to coordinate communication with Fuchsia devices. This
makes it more complex to write automated tests that use `ffx` since the configuration
and daemon should be isolated in order to avoid side effects, or interference from
the global environment.

To achieve this isolation, test authors need to use [isolate directories][isolate-dir]
when running tests which use `ffx`.

## Next steps

- Please provide feedback on this doc by [reaching out to the ffx team](#contacting_the_ffx_team)!
- Learn how to [extend `ffx`](/docs/development/tools/ffx/development/README.md).


<!-- Reference links -->

[isolate-dir]: development/integration_testing/README.md
