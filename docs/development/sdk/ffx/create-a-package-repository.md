# Create a Fuchsia package repository

The [`ffx repository`][ffx-repository] commands can create and manage
Fuchsia package repositories on the host machine.

## Concepts

When a Fuchsia device needs to run new software or update existing software,
the device requests and downloads [Fuchsia packages][fuchsia-package] from the
[Fuchsia package server][fuchsia-package-server], which is a service
that you can start (and stop) on the host machine. The Fuchsia package server
then serves Fuchsia packages from a
[Fuchsia package repository](#create-a-package-repository) configured
on the host machine.

A Fuchsia package repository is mapped to a directory on the host machine.
When handling requests from Fuchsia devices, the Fuchsia package server looks
for Fuchsia packages in this directory and, when they're found, it serves the
packages from this directory. Therefore, this directory is often the
destination where you'd generate and store Fuchsia packages related to your
development.

Once a new Fuchsia package repository is created, you need to register the
package repository to your Fuchsia device (or devices), which permits the device
to download Fuchsia packages from this package repository. Fuchsia devices can
only download Fuchsia packages from their registered Fuchsia package repositories.

Lastly, you can set up the Fuchsia package server to serve from multiple Fuchsia
package repositories on the same host machine, where you may dedicate each package
repository for a specific purpose (for instance, to separate stable packages
from experimental packages).

## Create a package repository {:#create-a-package-repository}

1. Create a new repository:

   ```posix-terminal
   ffx repository create <REPO_PATH>
   ```

   where `REPO_PATH` is the directory path for the new repository.
   Note: There is no check for existing contents in that directory, so it is
   recommended that you check that the directory does not exist before running
   this command.

1. Optionally set the default name for the repository:

   ```posix-terminal
   ffx repository default set <REPO_NAME>
   ```

   Replace `REPO_NAME` with the name of a repository.

   The example command below sets `my-repo` to be the default repository:

   ```none {:.devsite-disable-click-to-copy}
   ffx repository default set my-repo
   ```

   This is typically not needed since the build directory level configuration of
   ffx will already contain the correct value.

   After setting the default repository, this command exits silently without
   output.

## Start the Fuchsia package server

To be able to serve packages from Fuchsia package repositories
on your host machine, the Fuchsia package server must be running
on the machine.

To start the Fuchsia package server, run the following command:

```posix-terminal
ffx repository server start --foreground  --repo-path <REPO_PATH>
```

This will start a package server in the foreground. The log messages will appear
in the terminal window.

Alternatively, the server can be started in the background by using the
`--background` flag. In this case the log messages will appear in
`${log.dir}/repo-REPO_NAME.log`.

For more options, see [Start package servers][start-package-servers].

## Start a package server based on a product bundle

If you need to serve packages from an existing product bundle. The product
bundle can be downloaded and the package server started using the product bundle.

1. To see the list of available products, run the following command:

    ```posix-terminal
    ffx product list
    ```

2. To download your desired product bundle, run the following command:

    ```posix-terminal {:.devsite-disable-click-to-copy}
    ffx product download <PRODUCT> <DOWNLOAD_DIR>
    ```

    Replace `PRODUCT` and `DOWNLOAD_DIR` with your desired product and download
    directory, for example:

    ```posix-terminal
    ffx product download core.x64 ~/Downloads/product-bundles/
    ```

3. To start a Fuchsia package server, run the following command:

    ```posix-terminal {:.devsite-disable-click-to-copy}
    ffx repository server start --background --product-bundle <DOWNLOAD_DIR>
    ```

    Replace the following:

    * `DOWNLOAD_DIR`: Is the directory that contains a product bundle. You can
        confirm it is a product bundle directory by the presence of
        _DOWNLOAD_DIR/product.bundle.json_.

    For more options, see [Start package servers][start-package-servers].

## Register a package repository to a Fuchsia device {:#register-a-package-repository}

Once the package server is running, the server address needs to be registered on
the target device.

1. Enable your Fuchsia device to connect to the new repository:

   ```posix-terminal
   ffx target repository register [-r <REPO_NAME>] --alias fuchsia.com --alias chromium.org
   ```

   Replace `REPO_NAME` with the name of the repository that you want the
   Fuchsia device to connect to. If this flag is not specified, the command
   selects the default repository.

   The example below sets your current Fuchsia device to connect to the default
   repository (`my-repo`) at `fuchsia.com`:

   ```none {:.devsite-disable-click-to-copy}
   $ ffx target repository register --alias fuchsia.com --alias chromium.org
   ```

   After registering the repository, this command exits silently without output.

1. Verify that the new repository is registered:

   ```posix-terminal
   ffx target repository list
   ```

   This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx target repository list
REPO                  URL                              ALIASES
fuchsia-pkg://default ["http://10.0.2.2:8083/my-repo"] ["fuchsia.com", "chromium.org"]
```

## Deregister a package repository {:#deregister-a-package-repository}

To deregister a Fuchsia package repository from the device,
run the following command:

```posix-terminal
ffx target repository deregister [-r <REPO_NAME>]
```

Replace `REPO_NAME` with the name of a registered repository. If this flag is
not specified, the command selects the default repository.

The example command below deregisters the `my-repo` repository:

```none {:.devsite-disable-click-to-copy}
$ ffx target repository deregister -r my-repo
```

## Stop the Fuchsia package server {:#stop-the-fuchsia-package-server}

To stop the Fuchsia package server, run the following command:

```posix-terminal
ffx repository server stop
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx repository server stop
server stopped
```

For more options, see
[Stop running package servers][stop-package-servers].

## Create a package repository for the deprecated daemon based server

Note: The daemon based package server is deprecated. If you must use it, please
[file a bug](https://issues.fuchsia.dev/issues/new?component=1378294&template=1838957).

To create a new Fuchsia package repository on your host machine,
do the following:

1. Create a new repository:

   ```posix-terminal
   ffx repository create <REPO_PATH>
   ```

   where `REPO_PATH` is the directory path for the new repository.
   Note: There is no check for existing contents in that directory, so it is
   recommended that you check that the directory does not exist before running
   this command.

1. Add the new repository to the ffx configuration:

   ```posix-terminal
   ffx repository add-from-pm <PM_REPO_PATH> [-r <REPO_NAME>]
   ```

   Replace the following:

   * `PM_REPO_PATH`: The path to a directory where Fuchsia packages are stored.
   * `REPO_NAME`: A user-defined name for the new repository.
     * If this flag is not specified, the command names the new repository `devhost`
       by default.

   The example command below creates a new repository and adds it with the
   name `my-repo`:

   ```none {:.devsite-disable-click-to-copy}
   $ ffx repository create ~/my-fuchsia-packages
   $ ffx repository add-from-pm ~/my-fuchsia-packages -r my-repo
   ```

   After creating a new repository, this command exits silently without output.

1. Verify that the new repository is created:

   ```posix-terminal
   ffx repository list
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ ffx repository list
   NAME                TYPE       ALIASES     EXTRA
   my-repo             pm                     /usr/alice/my-fuchsia-packages
   ```

1. Optionally set the new repository to be default:

   ```posix-terminal
   ffx repository default set <REPO_NAME>
   ```

   Replace `REPO_NAME` with the name of a repository.

   The example command below sets `my-repo` to be the default repository:

   ```none {:.devsite-disable-click-to-copy}
   $ ffx repository default set my-repo
   ```

   This is typically not needed since the build directory level configuration of
   ffx will already contain the correct value.

   After setting the default repository, this command exits silently without output.

   For a Fuchsia device to stat downloading Fuchsia packages from this new
   repository, you need to
   [register this repository to the device](#register-a-package-repository).

## Register a package repository to a Fuchsia device {:#register-a-package-repository}

To enable a Fuchsia device to download packages from a Fuchsia package repository
on your host machine, do the following:

Note: For Fuchsia package repositories to be able to serve packages to
Fuchsia devices, the [Fuchsia package server](#start-the-fuchsia-package-server)
needs to be running on the host machine.

1. Enable your Fuchsia device to connect to the new repository:

   ```posix-terminal
   ffx target repository register [-r <REPO_NAME>] --alias fuchsia.com --alias chromium.org
   ```

   Replace `REPO_NAME` with the name of the repository that you want the Fuchsia device
   to connect to. If this flag is not specified, the command selects the default repository.

   The example below sets your current Fuchsia device to connect to the default
   repository (`my-repo`) at `fuchsia.com`:

   ```none {:.devsite-disable-click-to-copy}
   $ ffx target repository register --alias fuchsia.com --alias chromium.org
   ```

   After registering the repository, this command exits silently without output.

1. Verify that the new repository is registered:

   ```posix-terminal
   ffx target repository list
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ ffx target repository list
   REPO                  URL                              ALIASES
   fuchsia-pkg://default ["http://10.0.2.2:8083/my-repo"] ["fuchsia.com", "chromium.org"]
   ```

## Deregister a package repository {:#deregister-a-package-repository}

To deregister a Fuchsia package repository from the device,
run the following command:

```posix-terminal
ffx target repository deregister [-r <REPO_NAME>]
```

Replace `REPO_NAME` with the name of a registered repository. If this flag is
not specified, the command selects the default repository.

The example command below deregisters the `my-repo` repository:

```none {:.devsite-disable-click-to-copy}
$ ffx target repository deregister -r my-repo
```

After deregistering the repository, this command exits silently without output.

## Remove a package repository {:#remove-a-package-repository}

To remove a Fuchsia package repository, run the following command:

```posix-terminal
ffx repository remove <REPO_NAME>
```

Replace `REPO_NAME` with the name of a repository.

The example command below removes the `my-repo` repository:

```none {:.devsite-disable-click-to-copy}
$ ffx repository remove my-repo
```

After removing the repository, this command exits silently without output.

## Start the Fuchsia package server {:#start-the-fuchsia-package-server}

To be able to serve packages from Fuchsia package repositories
on your host machine, the Fuchsia package server must be running
on the machine.

To start the Fuchsia package server, run the following command:

```posix-terminal
ffx repository server start --daemon
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx repository server start --daemon
server is listening on [::]:8083
```

For more options, see [Start package servers][start-package-servers].

## Stop the Fuchsia package server {:#stop-the-fuchsia-package-server}

To stop the Fuchsia package server, run the following command:

```posix-terminal
ffx repository server stop
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx repository server stop
server stopped
```

For more options, see
[Stop running package servers][stop-package-servers].

<!-- Reference links -->

[ffx-repository]: https://fuchsia.dev/reference/tools/sdk/ffx#repository
[fuchsia-package]: /docs/concepts/packages/package.md
[fuchsia-package-server]: /docs/concepts/packages/fuchsia_package_server.md
[start-package-servers]: start-package-servers.md
[stop-package-servers]: stop-package-servers.md
