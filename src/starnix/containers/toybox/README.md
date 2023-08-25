# Toybox Container

You can use `docker` to create an interactive container with the `toybox` utilities.

## Setup

To get started, use `docker` to download the container and save it as a TAR file:

```posix-terminal
sudo docker pull tianon/toybox
sudo docker save tianon/toybox:latest -o local/toybox.tar
sudo chmod a+r local/toybox.tar

Then, use `create_container_package` to convert the container into a Fuchsia package. Before
running this command, make sure to build Fuchsia using an `fx set` command that has
`--with //src/starnix`:

```posix-terminal
fx host-tool create_container_package --input-format docker-archive local/toybox.tar local/toybox.far
```

## Publish

After you build Fuchsia, you can publish the Fuchsia `toybox` Fuchsia package to your local
package repository:

```posix-terminal
ffx repository publish "$(fx get-build-dir)/amber-files" --package-archive local/toybox.far
```

You will need to repeat this command each time you rebuild Fuchsia because rebuilding Fuchsia
recreates your local package repository.

## Run

You can now run the `toybox` container:

```posix-terminal
ffx component run /core/starnix_runner/playground:toybox fuchsia-pkg://fuchsia.com/toybox#meta/container.cm
```

There is nothing special about running `toybox` in this `playground` collection. You can run
`toybox` anywhere in the component topology that has the `starnix` runner capability.

Once the container is running, you can use the `console` command to get an interactive console:

```posix-terminal
ffx starnix console --moniker /core/starnix_runner/playground:toybox /bin/sh
```
