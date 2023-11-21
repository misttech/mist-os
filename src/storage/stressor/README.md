# Stressor

This stressor is designed to be run as a component. It continually stresses the filesystem (whatever
persistent storage happens to be configured) with various random operations. To include it, simply
add the following to your `fx set` invocation:

```
--with-base //src/storage/stressor --args 'core_realm_shards+=["//src/storage/stressor:core_shard"]'
```

You can monitor its progress using `fx log`.
