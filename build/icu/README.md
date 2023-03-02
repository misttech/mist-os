# ICU library utilities for Fuchsia

## Templates for ICU deployment on Fuchsia

The *.gni templates are shims around templates with the same name, with
`icu_` prepended.

These are used to produce artifacts that use different commit IDs for the  ICU
library for product assembly.

For example, `icu_source_set` is a `source_set` target that produces 3 flavors
of each target it is instantiated for.

For example, `icu_source_set("foo")` produces several targets named
`foo.icu_XXXX`, where `XXXX` is replaced by the commit ID of the ICU library
used in its compilation.
