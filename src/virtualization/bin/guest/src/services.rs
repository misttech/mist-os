// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::{Context, Error},
    fidl_fuchsia_virtualization::{GuestManagerMarker, GuestManagerProxy},
    fuchsia_async::{self as fasync},
    fuchsia_component::client::connect_to_protocol_at_path,
    std::os::unix::{io::AsRawFd, io::FromRawFd, prelude::RawFd},
};

fn set_fd_to_unblock(raw_fd: RawFd) -> () {
    // SAFETY: This is unsafe purely due to FFI. There are no assumptions
    // about this code.
    unsafe {
        libc::fcntl(raw_fd, libc::F_SETFL, libc::fcntl(raw_fd, libc::F_GETFL) | libc::O_NONBLOCK)
    };
}

pub enum Stdio {
    Stdin,
    Stdout,
    Stderr,
}

impl AsRawFd for Stdio {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Stdio::Stdin => std::io::stdin().as_raw_fd(),
            Stdio::Stdout => std::io::stdout().as_raw_fd(),
            Stdio::Stderr => std::io::stderr().as_raw_fd(),
        }
    }
}

pub unsafe fn get_evented_stdio(stdio: Stdio) -> fasync::net::EventedFd<std::fs::File> {
    // SAFETY: This method returns an EventedFd that wraps around a file linked to std{in,out,err}
    // This method should only be called once for each type, as having multiple files that
    // are tied to a given FD can cause conflicts.

    set_fd_to_unblock(stdio.as_raw_fd());
    // SAFETY: EventedFd::new() is unsafe because it can't guarantee the lifetime of
    // the file descriptor passed to it exceeds the lifetime of the EventedFd.
    // Stdin, stdout, and stderr should remain valid for the lifetime of the program.
    // File is unsafe due to the from_raw_fd assuming it's the only owner of the
    // underlying object; this may cause memory unsafety in cases where one
    // relies on this being true, which we handle by using a reference where this matters
    //
    // Note that since File takes ownership of the fd, the fd will be closed when the EventedFd
    // is dropped. This behaviour could be avoided by using a std::io::Stdin (etc.) directly, but
    // they are buffered which may be undesirable.
    fasync::net::EventedFd::new(std::fs::File::from_raw_fd(stdio.as_raw_fd())).unwrap()
}

pub fn connect_to_manager(
    guest_type: crate::arguments::GuestType,
) -> Result<GuestManagerProxy, Error> {
    let manager = connect_to_protocol_at_path::<GuestManagerMarker>(
        format!("/svc/{}", guest_type.guest_manager_interface()).as_str(),
    )
    .context("Failed to connect to manager service")?;
    Ok(manager)
}
