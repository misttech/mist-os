# Deleting drivers

When deleting a driver:

1. Get approval from at least one OWNER of the driver. If all of the OWNERS have abandoned the
driver and are not responding, then an OWNER higher in the Fuchsia tree needs to approve the CL
that performs the deletion.
1. Add an entry in the [driver epitaphs][driver-epitaphs] file for each driver deleted,
including a git hash for the fuchsia.git repository that contained the most up to date version
of the driver before being deleted. The following information must be provided:

- Short_description: Provides a description of the deleted driver.
- Tracking_bug: A Monorail issue that describes the reason for the driver's deletion.
- Gerrit_change_id: The ID of the Gerrit change used to delete the driver.
- Available_in_git: fuchsia.git SHA that includes the most up to date version of the driver.
- Path: The path of the deleted driver.

For example:

```
short_description: 'Qualcomm Peripheral Image Loading driver'
tracking_bug: '123456'
gerrit_change_id: '506858'
available_in_git: 'f441460db6b70ba38150c3437f42ff3d045d2b71'
path: '/src/devices/fw/drivers/qcom-pil'
```

[driver-epitaphs]: /docs/reference/hardware/_drivers_epitaphs.yaml
