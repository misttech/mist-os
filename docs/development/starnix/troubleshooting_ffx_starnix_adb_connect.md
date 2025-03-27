# Troubleshooting ffx starnix adb connect

This page provides troubleshooting tips for making an ADB connection to
Fuchsia devices.

If you cannot use the `adb` command to connect to your Fuchsia device over
USB, you can run the following command to make a device running Android
inside Starnix visible to the ADB server on your host machine:

```posix-terminal
ffx starnix adb connect
```

This command enables `adb` to connect to your device using TCP and makes use of
a network address provided by `ffx` for the connection.

However, there are a few ways this command can fail to work, and this page
includes guidance for addressing common issues.

## Multiple adb devices

When multiple Android devices are connected to your development machine or when
the Android device is running in an emulator, you may see an error from `adb`
that multiple devices are present and it doesn't know which one to use.

To verify if your development machine's ADB server sees multiple devices,
run the following command:

```posix-terminal
adb devices -l
```

If the output lists multiple devices, you need to specify a target device
for your `adb` commands.

To find the identifier of your device, run the following `ffx` command:

```posix-terminal
ffx starnix adb connect
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx starnix adb connect
adb is connected!
See https://fuchsia.dev/go/troubleshoot-adb-connect if it doesn't work.
This connection's "serial number" for adb is {{ '<strong>' }}<ADB_CONNECTION_ADDRESS>{{ '</strong>' }}.
```

Take the serial number from this command and pass it as an
argument to your `adb` commands, for example:

```posix-terminal
adb -s {{ '<var>' }}ADB_CONNECTION_ADDRESS{{ '</var>' }} shell ls
```

Or you can set it as the `ANDROID_SERIAL` environment variable in your
terminal, which will be then used by subsequent `adb` commands:

```posix-terminal
export ANDROID_SERIAL={{ '<var>' }}ADB_CONNECTION_ADDRESS{{ '</var>' }}
```

Once this environment variable is set, you can use `adb` commands normally.
