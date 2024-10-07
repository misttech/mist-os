# Crash reporting

For development, it is often easier to dump the crash information in the logs as
the crash happens on device. For devices in the field, we want to be able to
send a report to a remote crash server as well as we might not have access to
the devices' logs. We use
[Crashpad](https://chromium.googlesource.com/crashpad/crashpad/+/HEAD/README.md)
as the third-party client library to talk to the remote crash server.

We control via JSON configuration files whether we upload the reports to a crash
server and if so, to which crash server. By default, we create a report, but we
do not upload it.

## Testing

To test your changes, on a real device, we have some unit tests and some helper
programs to simulate various crashes. For the helper programs, you first need to
apply a developer override to enable uploads on an eng device. For example, the
userdebug config enables crash report uploading if the privacy settings also
have it enabled.

### Developer overrides

First, define developer overrides in `//local/BUILD.gn`. This file is
Git-ignored so you can add it once and only use/modify as needed:

```
import("//build/assembly/developer_overrides.gni")

assembly_developer_overrides("feedback_upload_config") {
  developer_only_options = {
    forensics_options = {
      build_type_override = "eng_with_upload"
    }
  }
}

assembly_developer_overrides("feedback_userdebug_config") {
  developer_only_options = {
    forensics_options = {
      build_type_override = "userdebug"
    }
  }
}

assembly_developer_overrides("feedback_user_config") {
  developer_only_options = {
    forensics_options = {
      build_type_override = "user"
    }
  }
}
```

You can apply an override via `fx set` using
`--assembly-override <assembly_target_pattern>=<overrides_target>`. Note that
the "assembly_target_pattern" will be different for most products:

```sh
(host)$ fx set core.x64 --assembly-override //build/images/fuchsia/*=//local:feedback_userdebug_config
```

Note, the above overrides only work for eng builds and product assembly will fail
if used on user/userdebug builds.

Then, after running each one of the helper programs (see commands in sections
below), you should then look each time for the following line in the syslog:

```sh
(host)$ fx syslog --tag crash
...
successfully uploaded report at $URL...
...
```

Click on the URL (contact OWNERS if you don't have access and think you should)
and check that the report matches your expectations, e.g., the new annotation is
set to the expected value.

### Unit tests

To run the unit and integration tests:

```sh
(host) $ fx test crash-reports-tests
```

### Kernel crash

The following command will cause a kernel panic:

```sh
(target)$ k crash
```

The device will then reboot and the system should detect the kernel crash log,
attach it to a crash report and try to upload it. Look at the syslog upon
reboot.

### C userspace crash

The following command will cause a write to address 0x0:

```sh
(target)$ crasher
```

You can immediately look at the syslog.

## Question? Bug? Feature request?

Contact OWNERS.
