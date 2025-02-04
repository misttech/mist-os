# Troubleshooting `ffx starnix adb connect`

If your device isn't able to use `adb` over USB, you can use the
`ffx starnix adb connect` command to make a device running Android inside
Starnix visible to the `adb` server on your development machine.

This command asks `adb` to connect to your device using TCP, and makes use of
a network address provided by `ffx` for the connection.

There are a few ways this command can fail to work, this page includes guidance
for addressing the common cases.

## Multiple `adb` devices

When multiple Android devices are connected to your development machine or when
the Android device is running in an emulator, you may see an error from `adb`
that multiple devices are present and it doesn't know which one to use.

To verify if your development machine's `adb` server sees multiple devices, run:

```posix-shell
adb devices -l
```

If the output lists multiple devices, you need to specify one for your `adb`
commands.

To find the identifier of your device, run:

```posix-shell
ffx starnix adb connect
```

You should see output like:

```
...
adb is connected!
See https://fuchsia.dev/go/troubleshoot-adb-connect if it doesn't work.
This connection's "serial number" for adb is {{ '<var>' }}ADB_CONNECTION_ADDRESS{{ '</var>' }}.
```

Take the serial number printed by the command above and either pass it as an
argument to your `adb` commands:

```posix-shell
adb -s {{ '<var>' }}ADB_CONNECTION_ADDRESS{{ '</var>' }} shell ...
```

Or set an environment variable in your shell that will be used by subsequent
`adb` commands:

```posix-shell
export ANDROID_SERIAL={{ '<var>' }}ADB_CONNECTION_ADDRESS{{ '</var>' }}
adb shell ...
```
