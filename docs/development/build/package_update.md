# Developing with Fuchsia packages

This document explains the basics of Fuchsia product development using
[Fuchsia packages][packages].

## Overview

Almost everything that exists on a Fuchsia system is a [Fuchsia package][packages].
Whether it is immediately apparent or not, almost everything you see on
Fuchsia lives in a package.

During development, if you want to make new software available to your Fuchsia
device, you can build a Fuchsia package to include changes and publish it
to a local Fuchsia package server running on your development host. Then the
Fuchsia device will download the package and update the software.

In this setup, your development host will run a simple static file HTTP server
that makes Fuchsia packages available to the target device. This HTTP server
(which is referred to as the [Fuchsia package server][fuchsia-package-server])
is part of the Fuchsia source code and is built automatically.

Fuchsia target devices are usually configured to look for changes on the
development host. When the update system on a target device sees changes,
it will fetch the new package from the Fuchsia package server. Once the
package is downloaded, the new software in the package becomes available
on the target device.

## Prerequisites

Your development host and Fuchsia target device must be able to communicate over
TPC/IP. In particular, it must be possible to establish an SSH connection from
the development host to the target device. This SSH connection is used to issue
commands to the target device.

## Building packages

For building a package containing your code, you need to specify a package-type
build rule. If build rules need to be added for your package, see
[Fuchsia build system][pkg-doc].

Once an appropriate build rule is available, running `fx build` will generate
your target package.

## Inspecting packages

Fuchsia comes with the `ffx` tool to work with package archives. After the
completion of a [Fuchsia build][fuchsia-build], the
[`ffx package archive`][ffx-package-archive] command can be used to create,
list, dump or extract package archives:

```posix-terminal
ffx package archive list
```

For example, the `ffx package archive list` command below lists the contents
of a package named `meta.far` in the
`~/fuchsia/out/default/obj/third_party/sbase/sed_pkg/` directory:

```none {:.devsite-disable-click-to-copy}
$ ffx package archive list ~/fuchsia/out/default/obj/third_party/sbase/sed_pkg/meta.far
+-------------------------------+
| NAME                          |
+===============================+
| meta/contents                 |
+-------------------------------+
| meta/fuchsia.abi/abi-revision |
+-------------------------------+
| meta/package                  |
+-------------------------------+
```

Notice that this `meta.far` package contains a file named `meta/contents`.
The `meta/contents` file maps the user-facing filenames of the package to
the merkle root of those files, for example:

```none {:.devsite-disable-click-to-copy}
$ ffx package archive cat ~/fuchsia/out/default/obj/third_party/sbase/sed_pkg/meta.far meta/contents
bin/sed=361c6e3a027cf38af0d53e1009ef3b3448f4aeb60270910a116396f5ec8eccba
lib/ld.so.1=c8ec55a14590e7e6f06f311d632744012f85084968d98a335c778f29207caa14
lib/libc++.so.2=d8a45259c74cf65c1f8470ac043c6459403fdd0bbf676092ba82f36451e4e571
lib/libc++abi.so.1=f12b37c2b9bfb43fbe33d446794a6ffeee1452b16b7c8d08b3438f3045d1f19a
lib/libfdio.so=9834d890adedc8e532899a673092271e394458e98e6cee049cb78f5a95372b11
lib/libunwind.so.1=89e5dee07ff8d2a03c58d524d8d832e79c70dcb81508f8a83c6fbf07e1539e83
```

For more details on packages and their contents, see
[Structure of a package][pkg-struct].

## Package servers

Virtually all software running on a Fuchsia system is collected into
[Fuchsia packages][packages]. Beyond the base packages that comprise the
foundation of the Fuchsia platform, additional packages can be downloaded from
a [Fuchsia package server][fuchsia-package-server]. The Fuchsia package server
is an HTTP(S) server managing the Fuchsia packages using TUF (the update
framework). This framework uses cryptographically signed BLOBs to securely
distribute updated packages to a device running Fuchsia.

For information about how to start, stop, and list package servers running
in your development environment, see:

* [Start package servers](/docs/development/sdk/ffx/start-package-servers.md)
* [List running package servers](/docs/development/sdk/ffx/list-package-servers.md)
* [Stop running package servers](/docs/development/sdk/ffx/stop-package-servers.md)

## Connecting host and target

The Fuchsia source contains a simple HTTP server that serves static files. The
build generates and serves a [TUF][TUF-home] file tree.

The update agent on the target does not initially know where to look for
updates. To connect the agent on the target to the HTTP server running on the
development host, it must be told the IP address of the development host.

By default, the repository server will automatically register itself with the
default target. This behavior can be disabled by using the `--no-device` option
when starting the server.

The registration can be:

* Inspected using [`ffx target repository list`][ffx-target-repo-list]
* Updated using [`ffx target repository register`][ffx-target-repo-register]
* Removed using [`ffx target repository deregister`][ffx-target-repo-deregister]

The host HTTP server is started and the update agent is configured by calling `fx
serve -v`. `fx serve` will run the update server and is often what people use.
`-v` is recommended because the command will print more output, which may assist
with debugging. If the host connects successfully to the target you will see the
message `Ready to push packages!` in the shell on your host.

The update agent on the target will remain configured until it is reflashed or
persistent data is lost. The host will attempt to reconfigure the update agent
when the target is rebooted.

## Triggering package updates

In Fuchsia, packages are not "installed" but they are cached on an as-needed
basis.

There are two collections of packages in a Fuchsia ecosystem:

* **Base**: The `base` package set is a group of software critical to proper
  system function that must remain congruent. The Fuchsia build system
  assigns packages to `base` when they are provided to `fx set` using the
  `--with-base` flag.
  This set of software can only be updated by performing a whole system update,
  typically referred to as OTA, described below. This is updated using `fx ota`.

* **Ephemeral software**: Packages included in the `cache` or `universe` package
  set are ephemeral software, with updates delivered on demand. The Fuchsia
  build system assigns packages to `universe` when they are provided to `fx set`
  using the `--with` flag.
  Ephemeral software updates to the latest available version whenever the
  package URL is resolved.

## Triggering an OTA

Sometimes there may be many packages changed or the kernel may change or there
may be changes in the system package. To get kernel changes or changes in the
system package, an OTA or flashing is required as **base** packages are
immutable for the runtime of a system. An OTA update will usually be faster
than flashing the device.

The `fx ota` command asks the target device to perform an update from any of
the update sources available to it. To OTA update a build made on the dev host to
a  target on the same LAN, first build the system you want. If `fx serve [-v]`
isn't already running, start it so the target can use the development host as an
update source. The `-v` option will show more information about the files the
target is requesting from the host. If the `-v` flag was used there should
be a flurry of output as the target retrieves all the new files. Following
completion of the OTA the device will reboot.

## List of commands

The following commands are used for publishing packages and updating devices:

* [`fx serve [-v]`][fx-serve]: Run a local server for both build-push and OTA.
* [`fx ota`][fx-ota]: Trigger a full system update and reboot.
* [`fx test <COMPONENT_URL>`][fx-test]: Build and run tests.

## Issues and considerations

### You can fill up your disk

Every update pushed is stored in the content-addressed file system, blobfs.
Following a reboot the updated packages may not be available because the index
that locates them in blobfs is only held in RAM. The system currently does not
garbage collect inaccessible or no-longer-used packages (having garbage to
collect is a recent innovation!), but will eventually.

The `fx gc` command will reboot the target device and then evict all old
ephemeral software from the device, freeing up space.

### Restarting without rebooting

If the package being updated hosts a service managed by Fuchsia that service
may need to be restarted. Rebooting is undesirable both because it is slow and
because the package will revert to the version flashed on the device. Typically
a user can terminate one or more running components on the system, either by
asking the component to terminate gracefully, or by forcefully stopping the
component using `ffx component stop <component-moniker>`. Upon reconnection to
the component services, or by invocation via `ffx component start` or `fx test`,
new versions available in the package server will be cached before launch.

### Packaging code outside the Fuchsia tree

Packaging and pushing code that lives outside the Fuchsia tree is possible, but
will require more work. The Fuchsia package format is quite simple. It consists
of a metadata file describing the package contents, which is described in more
detail in the [Fuchsia package][pkg-struct] documentation. The metadata file is
added to a TUF file tree and each of the contents are named after their Merkle
root hash and put in a directory at the root of the TUF file tree called 'blobs'.

<!-- Reference links -->

[packages]: /docs/concepts/packages/README.md
[pkg-struct]: /docs/concepts/packages/package.md#structure-of-a-package
[TUF-home]: https://theupdateframework.github.io "TUF Homepage"
[pkg-doc]: /docs/development/build/build_system/fuchsia_build_system_overview.md "Build overview"
[fuchsia-build]: /docs/get-started/learn/build/build-system.md "Build system"
[ffx-target-repo-list]: https://fuchsia.dev/reference/tools/sdk/ffx#ffx_target_repository_list
[ffx-target-repo-register]: https://fuchsia.dev/reference/tools/sdk/ffx#ffx_target_repository_register
[ffx-target-repo-deregister]: https://fuchsia.dev/reference/tools/sdk/ffx#ffx_target_repository_deregister
[fuchsia-package-server]: /docs/concepts/packages/fuchsia_package_server.md
[ffx-package-archive]: https://fuchsia.dev/reference/tools/sdk/ffx#ffx_package_archive
[fx-serve]: https://fuchsia.dev/reference/tools/fx/cmd/serve
[fx-ota]: https://fuchsia.dev/reference/tools/fx/cmd/ota
[fx-test]: https://fuchsia.dev/reference/tools/fx/cmd/test
