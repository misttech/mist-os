# Stressor

This stressor is designed to be run as a component. It continually stresses the filesystem (whatever
persistent storage happens to be configured) with various random operations.

There are two variants that are different enough to warrant different binaries.

## Gentle variant

This is designed to run in the background and induce moderate disk activity without filling the
disk or otherwise interfering with system functionality. This variant opens, closes, reads,
writes, deletes and truncates files randomly but keeps the number of files and sizes to a minimum.
Sparse reads and writes are heavily exercised.

The aim of this variant is to exercise the filesystem in long-running healthy systems.

To include it, simply add the following to your `fx set` invocation:

```
--with-base //src/storage/stressor \
--args 'core_realm_shards+=["//src/storage/stressor:core_shard"]'
```

You can monitor its progress using `fx log`.

## Aggressive variant

This is designed to run in the background and induce significant device activity that is not cache
friendly. This will perform open-read-close, open-write-close and delete operations.
This version creates 10000 files to stay above `DIRENT_CACHE_LIMIT` (8000) and always reads from
the oldest file to ensure that reads don't end up hitting cached handles which may have a warm page
caches.

The aim of this variant here is to provide a load source that exacerbates fragmentation of files and
free space to allow for benchmarking of improvements in this area.

To include it, simply add the following to your `fx set` invocation:

```
$ fx set core.x64 \
    --with-base //src/storage/stressor \
    --args 'core_realm_shards+=["//src/storage/stressor:aggressive_core_shard"]'
```

You can monitor its progress using `fx log`.

Note that this variant is designed to fill up the disk so it may be useful to start the emulator
with a larger image size. e.g. 800MB:

```
$ fx build
$ IMAGE_SIZE=838860800 fx qemu -k -N -a x64 -c -y
```

You can inspect the logical and device IO ratios for the data partition via:

```
$ ffx inspect show bootstrap/fshost/fxfs
```
