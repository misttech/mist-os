# Test ChromiumOS Distro

This directory contains a minimal ChromiumOS distribution of Linux for use in
testing Starnix. The distribution contains a glibc-based userland with
sufficient machinery to run basic shell commands.

To run these tests, start `fx serve` in one terminal, and then, in another
terminal:

*Run all tests*
```
fx test test_chromiumos_syscalls
```

*Run selected tests*
```
fx test test_chromiumos_syscalls --test-filter="Input*"
```