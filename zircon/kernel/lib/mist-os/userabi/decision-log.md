## Why using vDSO infra from mist-os?

At first the vDSO(liblinux.so) is not used at runtime. We keep for the sake of consistency with the build structure
and we use it to validate the syscall ABI/API using the IFS file.

## Why using symlinks instead of copying files?

For this specified dir as the build infra is almost the same, some symlinks are used.
Only modified files were copied.


