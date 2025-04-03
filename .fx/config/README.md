# fx configs

Place configuration files that influence `fx` operation in this directory.

* `build-auth` reflects configuration that influences authentication for
  build services like RBE and ResultStore.  Settings are auto-detected,
  but can be manually overridden.  Used by `fx rbe auth` and `fx build`.
* `build-metrics` controls pushing of RBE logs and metrics to BigQuery.
  Set by `fx build-metrics`, and used by `fx build`.
* `build-profile` controls profiling local memory and network usage during
  build.  Set by `fx build-profile`, and used by `fx build`.
