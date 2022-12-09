This directory contains test data emulating third party projects directories,
used for testing that OWNERS files are generated correctly.

The directory structure contains:
* rust_medata.json: metadata file specifying the Rust 3P projects.
* manifest: XML file specifying the non-Rust 3P projects.
* owners.toml: specifies OWNERS files overrides.
* Rust projects:
  * third_party/rust_crates/vendor/foo: a project with an overridden OWNERS
    file.
  * third_party/rust_crates/vendor/bar: a project with an outdated OWNERS file.
* Non-rust projects:
  * third_party/foo: a project with an up-to-date OWNERS file.
  * third_party/bar: a project without an OWNERS file.
  * third_party/baz: a project without OWNERS that is depended on by file
    instead of gn target.
* dep/: project depending on all 3P projects.

