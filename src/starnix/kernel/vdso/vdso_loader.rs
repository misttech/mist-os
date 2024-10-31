// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::arch::vdso::VDSO_SIGRETURN_NAME;
use crate::mm::memory::MemoryObject;
use crate::mm::PAGE_SIZE;
use crate::time::utc::update_utc_clock;
use fuchsia_runtime::{UtcClockTransform, UtcInstant};
use once_cell::sync::Lazy;
use process_builder::elf_parse;
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, from_status_like_fdio, uapi};
use std::mem::size_of;
use std::sync::atomic::Ordering;
use std::sync::Arc;

static VVAR_SIZE: Lazy<usize> = Lazy::new(|| *PAGE_SIZE as usize);
pub static ZX_TIME_VALUES_MEMORY: Lazy<Arc<MemoryObject>> = Lazy::new(|| {
    load_time_values_memory().expect(
        "Could not find time values VMO! Please ensure /boot/kernel was routed to the starnix kernel.",
    )
});

#[derive(Default)]
pub struct MemoryMappedVvar {
    map_addr: usize,
}

impl MemoryMappedVvar {
    /// Maps the vvar memory to a region of the Starnix kernel root VMAR and stores the address of
    /// the mapping in this object.
    /// Initialises the mapped region with data by writing an initial set of vvar data
    pub fn new(memory: &MemoryObject) -> Result<MemoryMappedVvar, zx::Status> {
        let vvar_data_size = size_of::<uapi::vvar_data>();
        // Check that the vvar_data struct isn't larger than the size of the memory mapped vvar
        debug_assert!(vvar_data_size <= *VVAR_SIZE);
        let flags = zx::VmarFlags::PERM_READ
            | zx::VmarFlags::ALLOW_FAULTS
            | zx::VmarFlags::REQUIRE_NON_RESIZABLE
            | zx::VmarFlags::PERM_WRITE;
        let map_addr =
            memory.map_in_vmar(&fuchsia_runtime::vmar_root_self(), 0, 0, *VVAR_SIZE, flags)?;
        let memory_mapped_vvar = MemoryMappedVvar { map_addr };
        let vvar_data = memory_mapped_vvar.get_pointer_to_memory_mapped_vvar();
        vvar_data.seq_num.store(0, Ordering::Release);
        Ok(memory_mapped_vvar)
    }

    fn get_pointer_to_memory_mapped_vvar(&self) -> &uapi::vvar_data {
        let vvar_data = unsafe {
            // SAFETY: It is checked in the assertion in MemporyMappedVvar's constructor that the
            // size of the memory region map_addr points to is larger than the size of
            // uapi::vvar_data.
            &*(self.map_addr as *const uapi::vvar_data)
        };
        vvar_data
    }

    pub fn update_utc_data_transform(&self, new_transform: &UtcClockTransform) {
        let vvar_data = self.get_pointer_to_memory_mapped_vvar();
        let old_transform = UtcClockTransform {
            reference_offset: zx::MonotonicInstant::from_nanos(
                vvar_data.mono_to_utc_reference_offset.load(Ordering::Acquire),
            ),
            synthetic_offset: UtcInstant::from_nanos(
                vvar_data.mono_to_utc_synthetic_offset.load(Ordering::Acquire),
            ),
            rate: zx::sys::zx_clock_rate_t {
                synthetic_ticks: vvar_data.mono_to_utc_synthetic_ticks.load(Ordering::Acquire),
                reference_ticks: vvar_data.mono_to_utc_reference_ticks.load(Ordering::Acquire),
            },
        };
        if old_transform != *new_transform {
            let seq_num = vvar_data.seq_num.fetch_add(1, Ordering::Acquire);
            // Verify that no other thread is currently trying to update vvar_data
            debug_assert!(seq_num & 1 == 0);
            vvar_data
                .mono_to_utc_reference_offset
                .store(new_transform.reference_offset.into_nanos(), Ordering::Release);
            vvar_data
                .mono_to_utc_synthetic_offset
                .store(new_transform.synthetic_offset.into_nanos(), Ordering::Release);
            vvar_data
                .mono_to_utc_reference_ticks
                .store(new_transform.rate.reference_ticks, Ordering::Release);
            vvar_data
                .mono_to_utc_synthetic_ticks
                .store(new_transform.rate.synthetic_ticks, Ordering::Release);
            let seq_num_after = vvar_data.seq_num.swap(seq_num + 2, Ordering::Release);
            // Verify that no other thread also tried to update vvar_data during this update
            debug_assert!(seq_num_after == seq_num + 1)
        }
    }
}

impl Drop for MemoryMappedVvar {
    fn drop(&mut self) {
        // SAFETY: We owned the mapping.
        unsafe {
            fuchsia_runtime::vmar_root_self()
                .unmap(self.map_addr, *VVAR_SIZE)
                .expect("failed to unmap MemoryMappedVvar");
        }
    }
}

pub struct Vdso {
    pub memory: Arc<MemoryObject>,
    pub sigreturn_offset: u64,
    pub vvar_writeable: Arc<MemoryMappedVvar>,
    pub vvar_readonly: Arc<MemoryObject>,
}

impl Vdso {
    pub fn new() -> Self {
        let memory = load_vdso_from_file().expect("Couldn't read vDSO from disk");
        let sigreturn_offset = match VDSO_SIGRETURN_NAME {
            Some(name) => get_sigreturn_offset(&memory, name)
                .expect("Couldn't find sigreturn trampoline code in vDSO"),
            None => 0,
        };

        let (vvar_writeable, vvar_readonly) = create_vvar_and_handles();
        Self { memory, sigreturn_offset, vvar_writeable, vvar_readonly }
    }
}

fn create_vvar_and_handles() -> (Arc<MemoryMappedVvar>, Arc<MemoryObject>) {
    // Creating a vvar memory which has a handle which is writeable.
    let vvar_memory_writeable = Arc::new(MemoryObject::from(
        zx::Vmo::create(*VVAR_SIZE as u64).expect("Couldn't create vvar memory"),
    ));
    // Map the writeable vvar_memory to a region of Starnix kernel VMAR and write initial vvar_data
    let vvar_memory_mapped =
        Arc::new(MemoryMappedVvar::new(&vvar_memory_writeable).expect("couldn't map vvar memory"));
    // Write initial mono to utc transform to the vvar.
    update_utc_clock(&vvar_memory_mapped);
    let vvar_writeable_rights = vvar_memory_writeable.basic_info().rights;
    // Create a duplicate handle to this vvar memory which doesn't have write permission
    // This handle is used to map vvar into linux userspace
    let vvar_readable_rights = vvar_writeable_rights.difference(zx::Rights::WRITE);
    let vvar_memory_readonly = Arc::new(
        vvar_memory_writeable
            .duplicate_handle(vvar_readable_rights)
            .expect("couldn't duplicate vvar handle"),
    );
    (vvar_memory_mapped, vvar_memory_readonly)
}

fn sync_open_in_namespace(
    path: &str,
    flags: fidl_fuchsia_io::OpenFlags,
) -> Result<fidl_fuchsia_io::DirectorySynchronousProxy, Errno> {
    let (client, server) = fidl::Channel::create();
    let dir_proxy = fidl_fuchsia_io::DirectorySynchronousProxy::new(client);

    let namespace = fdio::Namespace::installed().map_err(|_| errno!(EINVAL))?;
    namespace.open_deprecated(path, flags, server).map_err(|_| errno!(ENOENT))?;
    Ok(dir_proxy)
}

/// Reads the vDSO file and returns the backing VMO.
fn load_vdso_from_file() -> Result<Arc<MemoryObject>, Errno> {
    const VDSO_FILENAME: &str = "libvdso.so";
    const VDSO_LOCATION: &str = "/pkg/data";

    let dir_proxy = sync_open_in_namespace(VDSO_LOCATION, fuchsia_fs::OpenFlags::RIGHT_READABLE)?;
    let vdso_vmo = syncio::directory_open_vmo(
        &dir_proxy,
        VDSO_FILENAME,
        fidl_fuchsia_io::VmoFlags::READ,
        zx::MonotonicInstant::INFINITE,
    )
    .map_err(|status| from_status_like_fdio!(status))?;

    Ok(Arc::new(MemoryObject::from(vdso_vmo)))
}

fn load_time_values_memory() -> Result<Arc<MemoryObject>, Errno> {
    const FILENAME: &str = "time_values";
    const DIR: &str = "/boot/kernel";

    let (client, server) = fidl::Channel::create();
    let dir_proxy = fidl_fuchsia_io::DirectorySynchronousProxy::new(client);

    let namespace = fdio::Namespace::installed().map_err(|_| errno!(EINVAL))?;
    namespace
        .open_deprecated(DIR, fuchsia_fs::OpenFlags::RIGHT_READABLE, server)
        .map_err(|_| errno!(ENOENT))?;

    let vmo = syncio::directory_open_vmo(
        &dir_proxy,
        FILENAME,
        fidl_fuchsia_io::VmoFlags::READ,
        zx::MonotonicInstant::INFINITE,
    )
    .map_err(|status| from_status_like_fdio!(status))?;

    // Check that the time values VMO is the expected size of 1 page. If it is not,
    // panic the kernel, as it means that the size of the time values VMO has changed
    // and the starnix vDSO linker script at //src/starnix/kernel/vdso/vdso.ld should
    // be updated.
    let vmo_size = vmo.get_size().expect("failed to get time values VMO size");
    let expected_size = 0x1000u64;
    if vmo_size != expected_size {
        panic!(
            "time values VMO has unexpected size; got {:?}, expected {:?}",
            vmo_size, expected_size
        );
    }
    Ok(Arc::new(MemoryObject::from(vmo)))
}

fn get_sigreturn_offset(vdso_memory: &MemoryObject, sigreturn_name: &str) -> Result<u64, Errno> {
    let vdso_vmo = vdso_memory.as_vmo().ok_or_else(|| errno!(EINVAL))?;
    let dyn_section = elf_parse::Elf64DynSection::from_vmo(vdso_vmo).map_err(|_| errno!(EINVAL))?;
    let symtab = dyn_section
        .dynamic_entry_with_tag(elf_parse::Elf64DynTag::Symtab)
        .ok_or_else(|| errno!(EINVAL))?;
    let strtab = dyn_section
        .dynamic_entry_with_tag(elf_parse::Elf64DynTag::Strtab)
        .ok_or_else(|| errno!(EINVAL))?;
    let strsz = dyn_section
        .dynamic_entry_with_tag(elf_parse::Elf64DynTag::Strsz)
        .ok_or_else(|| errno!(EINVAL))?;

    // Find the name of the signal trampoline in the string table and store the index.
    let strtab_bytes = vdso_vmo
        .read_to_vec(strtab.value, strsz.value)
        .map_err(|status| from_status_like_fdio!(status))?;
    let mut strtab_items = strtab_bytes.split(|c: &u8| *c == 0u8);
    let strtab_idx = strtab_items
        .position(|entry: &[u8]| std::str::from_utf8(entry) == Ok(sigreturn_name))
        .ok_or_else(|| errno!(ENOENT))?;

    const SYM_ENTRY_SIZE: usize = std::mem::size_of::<elf_parse::Elf64Sym>();

    // In the symbolic table, find a symbol with a name index pointing to the name we're looking for.
    let mut symtab_offset = symtab.value;
    loop {
        let sym_entry = vdso_vmo
            .read_to_object::<elf_parse::Elf64Sym>(symtab_offset)
            .map_err(|status| from_status_like_fdio!(status))?;
        if sym_entry.st_name as usize == strtab_idx {
            return Ok(sym_entry.st_value);
        }
        symtab_offset += SYM_ENTRY_SIZE as u64;
    }
}
