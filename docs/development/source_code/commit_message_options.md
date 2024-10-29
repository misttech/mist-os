# Commit message options

When uploading changes  to [Fuchsia's Gerrit instance][gerrit-link],
there are various options that can change the behavior of presubmit.
These options are represented by special strings added to the commit message
of a change.

This page documents the options that can be used for Fuchsia presubmit.

## Buganizer issue options

These options control associated bugs in [Buganizer][buganizer].

### Bug

`Bug: #` adds a comment on the given [Buganizer][buganizer] issue
when a change is submitted.

For example:

```none {:.devsite-disable-click-to-copy}
Bug: 372314445
```

The above line resulted in [this
comment](https://issues.fuchsia.dev/issues/372314445#comment11).

### Fixed

`Fixed: #` adds a comment, **and marks as "Fixed"**, the given
[Buganizer][buganizer] issue when a change is submitted.

For example:

```none {:.devsite-disable-click-to-copy}
Fixed: 297456438
```

The above line resulted in
[this comment](https://issues.fuchsia.dev/issues/297456438#comment3)
and status change on the issue.

## Test options

These options control how tests are executed.

### Multiply

`Multiply: <test name>` will run the given test multiple times.
This is helpful to confirm that specific tests are not flaky.

For example:

```none {:.devsite-disable-click-to-copy}
Multiply: socket-integration
```

The above line reruns the "socket-integration" test multiple times.

### Run-All-Tests

`Run-All-Tests: true` runs all tests, even if static analysis
marks them as unaffected by a change.  This option is helpful when
making changes that can implicitly affect the entire system, such
as changes to the Zircon kernel, Component Framework, Test Manager,
or Diagnostics.

### Cq-Include-Trybots

`Cq-Include-Trybots <list>` runs the given builders as part of presubmit
for the change, in addition to the default set of presubmit builders.

For example:

```none {:.devsite-disable-click-to-copy}
Cq-Include-Trybots: luci.fuchsia.try:fuchsia-coverage-absolute
```

The above line forces execution of `fuchsia-coverage-absolute`
along with the other jobs for presubmit.

Use the **Choose Tryjobs** dropdown in Gerrit to see the full set of
available builders.

<!-- Reference links -->

[buganizer]: https://issues.fuchsia.dev
[gerrit-link]: https://fuchsia-review.googlesource.com/
