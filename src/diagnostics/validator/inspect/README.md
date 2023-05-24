# Inspect Validator

Reviewed on: 2023-05-24

## Puppets

The Rust and C++ puppets live here, under `lib/cpp` and `lib/rust`.

The Dart puppet is at `//sdk/dart/fuchsia_inspect/test/validator_puppet`.

## Puppeteer

The puppeteer for the Inspect Validator is under `src/`. It exercises the various validation
puppets.

## Building

To build everything, append the following to your `fx set`:

```
 --with //src/diagnostics/validator/inspect:tests,//sdk/dart/validator_puppet:tests --with-base //src/dart  --args='core_realm_shards += [ "//src/dart:dart_runner_core_shard" ]'
```

Note: you will need to build and restart your emulator to get Dart support.

If you only want the C++ and Rust tests, use

```
--wtih //src/diagnostics/validator/inspect:tests
```

This will not require an emulator restart.

## Testing

To run the test:
```
fx test inspect-validator-test
```

Or, if set as above in the Building section, `fx test` alone will work.

