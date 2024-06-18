This directory contains utilities for fuzzing the Starnix system call interface
with [syzkaller](https://github.com/google/syzkaller).

The `start_sshd` component runs `sshd` on a Starnix container. Example usage:

- Build Fuchsia together with an Alpine container and the `start_sshd` helper: `fx set workbench_eng.x64 --with-base //src/testing/fuzzing/syzkaller/starnix:syzkaller_starnix && fx build`

- Start Fuchsia emulator: `ffx emu start --headless`

- Start Alpine container on Starnix runner: `ffx component run /core/starnix_runner/playground:alpine fuchsia-pkg://fuchsia.com/syzkaller_starnix#meta/alpine_container.cm`

- Start sshd: `ffx component run /core/starnix_runner/playground:alpine/daemons:start_sshd fuchsia-pkg://fuchsia.com/syzkaller_starnix#meta/start_sshd.cm`

- Copy ssh public key to container: `ffx component copy $(ffx config get ssh.pub | tr -d '"') /core/starnix_runner/playground:alpine::out::fs_root/tmp/authorized_keys`

- Forward the container's ssh port: `ssh -i $(ffx config get ssh.priv | tr -d '"') -L localhost:7000:localhost:7000 ssh://$(ffx target get-ssh-address)`

- ssh to container: `ssh -i $(ffx config get ssh.priv | tr -d '"') -p 7000 root@localhost`
