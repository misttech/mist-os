// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    auth::FsCred,
    fs::{tmpfs::*, *},
    task::*,
    types::*,
};

pub fn dev_tmp_fs(task: &CurrentTask) -> &FileSystemHandle {
    task.kernel().dev_tmp_fs.get_or_init(|| init_devtmpfs(task))
}

fn init_devtmpfs(current_task: &CurrentTask) -> FileSystemHandle {
    let fs = TmpFs::new_fs(current_task.kernel());
    let root = fs.root();

    // TODO(fxb/119437): Subscribe uevent to create dev nodes.
    let mkchr = |name, device_type| {
        root.create_node(current_task, name, mode!(IFCHR, 0o666), device_type, FsCred::root())
            .unwrap();
    };

    let mkblk = |name, device_type| {
        root.create_node(current_task, name, mode!(IFBLK, 0o666), device_type, FsCred::root())
            .unwrap();
    };

    let mkdir = |name| {
        root.create_node(current_task, name, mode!(IFDIR, 0o755), DeviceType::NONE, FsCred::root())
            .unwrap();
    };

    mkchr(b"kmsg", DeviceType::KMSG);
    mkchr(b"null", DeviceType::NULL);
    mkchr(b"zero", DeviceType::ZERO);
    mkchr(b"full", DeviceType::FULL);
    mkchr(b"random", DeviceType::RANDOM);
    mkchr(b"urandom", DeviceType::URANDOM);
    mkchr(b"fuse", DeviceType::FUSE);
    mkchr(b"loop-control", DeviceType::LOOP_CONTROL);
    root.create_symlink(current_task, b"fd", b"/proc/self/fd", FsCred::root()).unwrap();

    // TODO(fxbug.dev/128697): These devtmpfs entries should be populated automatically by
    // the loop-control device once devtmpfs is integrated with kobjects.
    mkblk(b"loop0", DeviceType::new(LOOP_MAJOR, 0));
    mkblk(b"loop1", DeviceType::new(LOOP_MAJOR, 1));
    mkblk(b"loop2", DeviceType::new(LOOP_MAJOR, 2));
    mkblk(b"loop3", DeviceType::new(LOOP_MAJOR, 3));
    mkblk(b"loop4", DeviceType::new(LOOP_MAJOR, 4));
    mkblk(b"loop5", DeviceType::new(LOOP_MAJOR, 5));
    mkblk(b"loop6", DeviceType::new(LOOP_MAJOR, 6));
    mkblk(b"loop7", DeviceType::new(LOOP_MAJOR, 7));
    mkblk(b"loop8", DeviceType::new(LOOP_MAJOR, 8));
    mkblk(b"loop9", DeviceType::new(LOOP_MAJOR, 9));
    mkblk(b"loop10", DeviceType::new(LOOP_MAJOR, 10));
    mkblk(b"loop11", DeviceType::new(LOOP_MAJOR, 11));
    mkblk(b"loop12", DeviceType::new(LOOP_MAJOR, 12));
    mkblk(b"loop13", DeviceType::new(LOOP_MAJOR, 13));
    mkblk(b"loop14", DeviceType::new(LOOP_MAJOR, 14));
    mkblk(b"loop15", DeviceType::new(LOOP_MAJOR, 15));
    mkblk(b"loop16", DeviceType::new(LOOP_MAJOR, 16));
    mkblk(b"loop17", DeviceType::new(LOOP_MAJOR, 17));
    mkblk(b"loop18", DeviceType::new(LOOP_MAJOR, 18));
    mkblk(b"loop19", DeviceType::new(LOOP_MAJOR, 19));
    mkblk(b"loop20", DeviceType::new(LOOP_MAJOR, 20));
    mkblk(b"loop21", DeviceType::new(LOOP_MAJOR, 21));
    mkblk(b"loop22", DeviceType::new(LOOP_MAJOR, 22));
    mkblk(b"loop23", DeviceType::new(LOOP_MAJOR, 23));

    mkdir(b"shm");

    // tty related nodes
    mkdir(b"pts");
    mkchr(b"tty", DeviceType::TTY);
    root.create_symlink(current_task, b"ptmx", b"pts/ptmx", FsCred::root()).unwrap();

    mkchr(b"fb0", DeviceType::FB0);
    fs
}
