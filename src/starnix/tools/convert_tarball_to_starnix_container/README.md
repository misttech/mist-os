# convert_tarball_to_starnix_container

This directory contains a tool to convert images of Linux containers (tar files) into Fuchsia
packages that can be run in Starnix.

## Creating a package

Be sure to have `//src/starnix/tools/convert_tarball_to_starnix_container` in
the GN graph, e.g.:
```
fx set workbench_eng.x64 --release --with //src/starnix/tools/convert_tarball_to_starnix_container
```

Then follow one of the following two subsections.

### From a tarball containing the root filesystem

Given a tar file with the contents of the root filesystem:
```
$ fx host-tool convert_tarball_to_starnix_container --input-format tarball \
    --arch x64 \
    ~/rootfs.tar ~/example.far
```

### From a Docker archive

Build the image as usual, e.g.:
```
$ docker build -t example .
```

Save it with `docker save`:
```
$ docker save example -o ~/example.tar
$ fx host-tool convert_tarball_to_starnix_container --input-format docker-archive \
    ~/example.tar ~/example.far
```

## Running the container

With `workbench_eng` and `fx serve` already running:
```
$ ffx repository publish "$(fx get-build-dir)/amber-files" --package-archive ~/example.far
$ ffx component run --recreate \
    /core/starnix_runner/playground:example \
    fuchsia-pkg://fuchsia.com/example#meta/container.cm
```

Launch an interactive shell in it:
```
$ ffx starnix console --moniker /core/starnix_runner/playground:example /bin/sh -i
```

Note: after running `fx build`, the `ffx repository publish ...` command must be
run again.

### Making the root filesystem writable

By default, the root filesystem of the container will be read-only.

To make it writable, add `--features rootfs_rw` to the
`convert_tarball_to_starnix_container` command line.

Changes will be stored in RAM and they will not persist after restarting the
container.
