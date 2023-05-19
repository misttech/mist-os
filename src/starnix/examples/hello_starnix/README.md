# Hello Starnix

This directory contains a simple Linux binary that runs in Starnix.

## How to run hello_starnix

First, start an empty container. This container does not have a disk image. Instead, the empty container mounts its own package as its root file system.  This container is useful for running self-contained binaries (e.g., that do not require `libc`):


```sh
ffx component run /core/starnix_runner/playground:starless fuchsia-pkg://fuchsia.com/starless#meta/empty_container.cm
```

You can confirm that this container is running using `ffx component list`. If everything has gone well, you should see `/core/starnix_runner/playground:starless` running in the system.

Next, run `hello_starnix` inside this container:

```sh
ffx component run --connect-stdio /core/starnix_runner/playground:starless/daemons:hello_starnix fuchsia-pkg://fuchsia.com/hello_starnix#meta/hello_starnix.cm
```

This command should produce the following output:

```
hello starnix
```
