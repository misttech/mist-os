This directory contains utilities for fuzzing the Starnix system call interface
with [syzkaller](https://github.com/google/syzkaller). The `start_sshd`
component runs `sshd` on a Starnix container, which can then be used to access
the container itself and run the Syzkaller binaries. This guide walks through
the process.

## Build and Run Starnix
Build Fuchsia with the syzkaller_starnix package.
```
fx set workbench_eng.x64 --with //src/testing/fuzzing/syzkaller/starnix:syzkaller_starnix && fx build
```
Start the Fuchsia emulator.
```
ffx emu start --headless
```
Kick off a package server.
```
fx serve
```
In another terminal, recreate the Alpine container with the Syzkaller Starnix runner.
```
ffx component run --recreate /core/starnix_runner/playground:alpine fuchsia-pkg://fuchsia.com/syzkaller_starnix#meta/alpine_container.cm
```

## Running Syzkaller Executables
You likely wish to run syzkaller executables, for instance to reproduce a bug.
Syzkaller bugs should include a small C program to reproduce. You can compile
that small program locally, like so:
```
gcc repro.c -lpthread -static -o repro.o
```
You'll now need to copy the executable to the target container. For example:
```
ffx component copy repro.o /core/starnix_runner/playground:alpine::out::fs_root/tmp
```
Next, we can drop into a shell on the container.
```
ffx starnix console --moniker /core/starnix_runner/playground:alpine /bin/bash
```
Unfortunately, we'll now need to copy the file once more to workaround a bug in
`ffx component copy` (b/325525342). The name of the copy is irrelevant.
```
cp tmp/repro.o tmp/repro.cp.o
```
Before running the binary, you'll likely want to start an `ffx log` stream from
another shell.
```
ffx log --since now
```
Lastly, we can execute the repro binary.
```
cd tmp/ && ./repro.cp.o
```

## [Alternative] SSHD Commands
The prior commands should work, but are technically not the same as what's run
in the Syzkaller infrastructure. If you need to replicate the infrastructure
more closely, you'll need to use SSHD to manage the file and shells. You'll
firstly follow the Build and Run Starnix section. But rather than use the ffx
tooling to copy and shell into the container, we'll use sshd. Firstly, we can
start the sshd daemon:
```
ffx component run /core/starnix_runner/playground:alpine/daemons:start_sshd fuchsia-pkg://fuchsia.com/syzkaller_starnix#meta/start_sshd.cm
```
Copy your ssh public key to the Starnix container.
```
ffx component copy $(ffx config get ssh.pub | tr -d '"') /core/starnix_runner/playground:alpine::out::fs_root/tmp/authorized_keys
```
Forward the container's ssh port.

NOTE: Depending on your environment, this and the following ssh / scp commands
may require `sudo` to correctly obtain access to resources. On gLinux machines,
you may receive permission errors if you do not execute as root.
```
ssh -f -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -i $(ffx config get ssh.priv | tr -d '"')  -NT -L 12345:127.0.0.1:7000 ssh://$(ffx target get-ssh-address)
```
Next, you can copy the file to the container:
```
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -i $(ffx config get ssh.priv | tr -d '"') -P 7000 repro.o root@localhost:/tmp
```
And access a shell within the container like so:
```
ssh -i $(ffx config get ssh.priv | tr -d '"') -p 7000 root@localhost
```
From here, you can run the executable like normal. Remember that you might want
to start an `ffx log --since now` stream prior to executing.
```
cd tmp/ && ./repro.o
```
