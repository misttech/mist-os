# create_container_package

This directory contains a tool to convert images of Linux containers (tar files) into Fuchsia
packages that can be run in Starnix.

## Creating a package

### From a tarball containing the root filesystem

Given a tar file with the contents of the root filesystem:
```
$ fx host-tool create_container_package --input-format tarball \
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
$ fx host-tool create_container_package --input-format docker-archive \
    ~/example.tar ~/example.far
```

## Running the container

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
