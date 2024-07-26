# stardev

The `stardev` container is intended for self-hosted development of Fuchsia.

## Status

Currently, only very basic software development is possible in `stardev`.

## Getting started


### Build Fuchia

In order to get reasonable performance in `stardev`, you should build Fuchsia using `--release`:

```sh
$ fx set workbench_eng.x64 --release
$ fx build
```

### Create the image

These commands use the `Dockerfile` in this directory to create the disk image for the container.
The image is stored in the `//local` directory as a Fuchsia Archive, called `stardev.far`:

```sh
$ docker build -t stardev src/starnix/containers/stardev
$ docker save stardev:latest -o local/stardev.tar
$ fx host-tool convert_tarball_to_starnix_container --features rootfs_rw --input-format docker-archive local/stardev.tar local/stardev.far
```

### Run the container

After building Fuchsia, you can add the `stardev` package to your local package repository and
then run the container in the Starnix playground:

```sh
$ ffx repository publish "$(fx get-build-dir)/amber-files" --package-archive local/stardev.far
$ ffx component run /core/starnix_runner/playground:stardev fuchsia-pkg://fuchsia.com/stardev#meta/container.cm
```

### Get a shell

Once you have the container running, you can connect to the container via the console:

```sh
$ ffx starnix console --moniker /core/starnix_runner/playground:stardev /bin/bash
```

### Installing software

Inside the container, you can install Debian packages using `apt-get` as normal:

```sh
# apt-get update
# apt-get install -y vim git clang
```

Currently, these packages get installed to file system that is backed by memory, which means they
are removed when the container is restarted. If you want to avoid installing these packages every
time you run the container, you can run `apt-get` from your `Dockerfile`, which will store the
packages in the persistent image.

## Hello, world

We can build and run a simple C program. First, create a directory and a `main.c` file:

```sh
# mkdir /tmp/hello_world
# cd /tmp/hello_world
# vim main.c
```

For example, you can use the following program:

```c
#include <stdio.h>

int main(int argc, char** argv) {
  printf("hello, starnix\n");
  return 0;
}
```

Next, build and run the program:

```sh
# clang -fuse-ld=/usr/bin/ld main.c -Wall -Werror -o hello_world
# ./hello_world
hello, starnix
```

TODO: Figure out why `-fuse-ld=/usr/bin/ld` is required. For some reason `clang` cannot find `ld`
on its own.
