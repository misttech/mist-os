# Inspect Validator Dart Puppet

Reviewed on: 2023-05-24

Inspect Validator exercises Inspect libraries and evaluates
the resulting VMOs for correctness and (soon) memory efficiency.

To do this, Validator controls "puppet" programs via FIDL, making them
do the actual VMO manipulations using the Inspect library in the target
languages.

This directory contains the Dart Puppet program. BUILD.gn's `:tests` target
invokes the Validator on the Dart Puppet, and is integrated in CQ/CI.

## Building

This project can be added to builds by including
`--with //sdk/dart/fuchsia_inspect/test/validator_puppet:tests`, along with necessary Dart support,
to the `fx set` invocation.

If the Dart support is added, as in the example `fx set` below, you will also need to build and
restart your emulator.

For example:

```
 fx set core.x64 --with //src/diagnostics/validator/inspect:tests,//sdk/dart/validator_puppet:tests --with-base //src/dart  --args='core_realm_shards += [ "//src/dart:dart_runner_core_shard" ]'
```

## Running

```
fx build && fx run-test inspect-validator-test-dart
```

## Testing

See Running. Since the Validator puppet integration tests completely
exercise the code in this Puppet, the Puppet does not include unit tests.

## Source layout

lib/main.rs contains the entry point main() which sets up a FIDL service and
dispatches initialization commands and actions to the Dart Inspect library.
