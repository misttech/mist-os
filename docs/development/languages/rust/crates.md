# Fuchsia Rust Crates

* [carnelian/](https://fuchsia-docs.firebaseapp.com/rust/carnelian/index.html)

    Carnelian is a prototype framework for writing Fuchsia applications in Rust.

* [fdio/](https://fuchsia-docs.firebaseapp.com/rust/fdio/index.html)

    Wrapper over zircon-fdio library

* [fuchsia-archive/](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_archive/index.html)

    Work with Fuchsia Archives (FARs)

* [fuchsia-async/](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_async/index.html)

    Fuchsia-specific Futures executor and asynchronous primitives (Channel, Socket, Fifo, etc.)

* [fuchsia-framebuffer/](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_framebuffer/index.html)

    Configure, create and use FrameBuffers in Fuchsia

* [fuchsia-merkle/](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_merkle/index.html)

    Protect and verify data blobs using [Merkle Trees](/docs/concepts/packages/merkleroot.md)

* [fuchsia-scenic/](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_scenic/index.html)

    Rust interface to Scenic, the Fuchsia compositor

* [fuchsia-syslog-listener/](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_syslog_listener/index.html)

    Implement fuchsia syslog listeners in Rust

* [fuchsia-syslog/](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_syslog/index.html)

    Rust interface to the fuchsia syslog

* [fuchsia-system-alloc/](/src/lib/fuchsia-system-alloc/)

    A crate that sets the Rust allocator to the system allocator. This is automatically included
    for projects that use fuchsia-async, and all Fuchsia binaries should ensure that they take a
    transitive dependency on this crate (and “use” it, as merely setting it as a dependency in GN
    is not sufficient to ensure that it is linked in).

* [fuchsia-trace/](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_trace/index.html)

    A safe Rust interface to Fuchsia's tracing interface

* [storage](/src/lib/storage/)

    Bindings and protocol for serving filesystems on the Fuchsia platform

    * [vfs](https://fuchsia-docs.firebaseapp.com/rust/vfs/index.html)

    A library to create “pseudo” file systems. These file systems are backed by
    in process callbacks. Examples are: component configuration, debug
    information or statistics.

    * [storage-manager](https://fuchsia-docs.firebaseapp.com/rust/storage_manager/index.html)

    A library to access a file system directory.

    * [fxfs](https://fuchsia-docs.firebaseapp.com/rust/fxfs/index.html)

    A library to us Fxfs which is a log-structured filesystem for Fuchsia. 

* [fuchsia-fs](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_fs/index.html)

    Wrapper library for fuchsia.io operations, such as reading and writing files, reading directory
    entries, and watching directories.

* [fuchsia-zircon/](https://fuchsia-docs.firebaseapp.com/rust/fuchsia_zircon/index.html)

    Rust language bindings for Zircon kernel syscalls

* [mapped-vmo/](https://fuchsia-docs.firebaseapp.com/rust/mapped_vmo/index.html)

    A convenience crate for Zircon VMO objects mapped into memory

* [mundane/](https://fuchsia-docs.firebaseapp.com/rust/mundane/index.html)

    A Rust crypto library backed by BoringSSL

* [shared-buffer/](https://fuchsia-docs.firebaseapp.com/rust/shared_buffer/index.html)

    Utilities for safely operating on memory shared between untrusting processes
