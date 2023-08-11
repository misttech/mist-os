Start the [Fuchsia emulator][femu] on the host machine and configure the
emulator instance to use Fuchsia’s new [driver framework][driver-framework]
(DFv2).

The tasks include:

*   Download a Fuchsia prebuilt image from Google Cloud Storage.
*   Start the Fuchsia emulator.
*   Set the emulator instance as your host machine’s default target device.
*   Start the Fuchsia package server.
*   Register the system package repository to the emulator instance.

Do the following:

1. Look up the manifest URL of the latest `core` prebuilt image on Google Cloud
   Storage:

   ```posix-terminal
   tools/ffx product lookup core.x64 14.20230811.2.1 --base-url gs://fuchsia/development/14.20230811.2.1
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx product lookup core.x64 14.20230811.2.1 --base-url gs://fuchsia/development/14.20230811.2.1
   Progress for "Getting product descriptions"
     development/14.20230811.2.1/product_bundles.json
       766 of 766 bytes (100.00%)
   {{ '<strong>' }}gs://fuchsia-public-artifacts-release/builds/8773051783254971601/transfer.json{{'</strong>' }}
   ```

1. Download the latest `core` prebuilt image using the manifest URL from step 1:

   ```posix-terminal
   tools/ffx product download gs://fuchsia-public-artifacts-release/builds/8773051783254971601/transfer.json ~/local_pb --force
   ```

   This command may take a few minutes to download the product image and metadata. These
   artifacts are downloaded to the `~/local_pb` directory on the host machine.

1. Inform the package repository of the new packages:

   ```posix-terminal
   tools/ffx repository add ~/local_pb
   ```

   This command creates a local Fuchsia package repository (named `devhost.fuchsia.com`)
   on the host machine. This local package repository is used to host additional system
   packages stored  in the `~/local_pb` directory (which are needed to run the `core`
   prebuilt image). Later in step 10, you’ll manually register this package repository
   to the emulator instance.

1. Stop all emulator instances:

   ```posix-terminal
   tools/ffx emu stop --all
   ```

1. Start the Fuchsia emulator:

   ```posix-terminal
   tools/ffx emu start ~/local_pb --headless
   ```

   This command starts a headless emulator instance running the `core` prebuilt image.

   When the instance is up and running, the command prints output similar to
   the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx emu start ~/local_pb --headless
   Auto resolving networking to user-mode. For more information see https://fuchsia.dev/fuchsia-src/development/build/emulator#networking
   Logging to "/home/alice/.local/share/Fuchsia/ffx/emu/instances/fuchsia-emulator/emulator.log"
   Waiting for Fuchsia to start (up to 60 seconds)...........
   Emulator is ready.
   ```

1. Verify that the new emulator instance is running:

   ```posix-terminal
   tools/ffx emu list
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx emu list
   [running]  fuchsia-emulator
   ```

1. Set the default target device:

   ```posix-terminal
   tools/ffx target default set fuchsia-emulator
   ```

   This command exits silently without output.

1. Start the Fuchsia package server:

   ```posix-terminal
   tools/ffx repository server start
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx repository server start
   Repository server is listening on [::]:8083
   ```

1. Check the list of Fuchsia package repositories available on
   your host machine:

   ```posix-terminal
   tools/ffx repository list
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx repository list
   +----------------------+------------+-----------------+------------------------------------------------+
   | NAME                 | TYPE       | ALIASES         | EXTRA                                          |
   +======================+============+=================+================================================+
   | devhost.fuchsia.com* | filesystem | +-------------+ | +----------+---------------------------------+ |
   |                      |            | | fuchsia.com | | | metadata | /home/alice/local_pb/repository | |
   |                      |            | +-------------+ | +----------+---------------------------------+ |
   |                      |            |                 | | blobs    | /home/alice/local_pb/blobs      | |
   |                      |            |                 | +----------+---------------------------------+ |
   +----------------------+------------+-----------------+------------------------------------------------+
   ```

   Notice that a package repository (`devhost.fuchsia.com`) is created for the
   `core` prebuilt image.

1. Register the `devhost.fuchsia.com` package repository to the target device:

   ```posix-terminal
   tools/ffx target repository register -r devhost.fuchsia.com --alias fuchsia.com --alias chromium.org
   ```

   This command exits silently without output.

<!-- Reference links -->

[driver-framework]: /docs/concepts/drivers/driver_framework.md
[femu]: /docs/development/sdk/ffx/start-the-fuchsia-emulator.md
