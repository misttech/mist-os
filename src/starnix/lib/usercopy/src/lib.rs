// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::Range;

use fdio;
use fidl_fuchsia_io as fio;
use fuchsia_runtime;
use fuchsia_zircon as zx;
use zerocopy::FromBytes;
use zx::{AsHandleRef, HandleBased, Task};

#[cfg(target_arch = "x86_64")]
extern "C" {
    // This function generates a "return" from the usercopy routine with an error.
    fn hermetic_copy_error();
}

type HermeticCopyFn = unsafe extern "C" fn(
    dest: *mut std::ffi::c_void,
    source: *const std::ffi::c_void,
    len: usize,
) -> usize;

pub struct Usercopy {
    // Pointer to the hermetic_copy routine loaded into memory.
    hermetic_copy_fn: HermeticCopyFn,

    // This is an event used to signal the exception handling thread to shut down.
    shutdown_event: zx::Event,

    // Handle to the exception handling thread.
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl Usercopy {
    /// Returns a new instance of `Usercopy` if unified address spaces is
    /// supported on the target architecture.
    pub fn new(restricted_address_range: Range<usize>) -> Result<Option<Self>, zx::Status> {
        if cfg!(not(target_arch = "x86_64")) {
            return Ok(None);
        }

        let hermetic_copy_blob = fdio::open_fd(
            "/pkg/hermetic_copy.bin",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )?;

        let hermetic_copy_exec_vmo = fdio::get_vmo_exec_from_file(&hermetic_copy_blob)?;

        let hermetic_copy_size = hermetic_copy_exec_vmo.get_size()?;

        let hermetic_copy_mapped_addr = fuchsia_runtime::vmar_root_self().map(
            0,
            &hermetic_copy_exec_vmo,
            0,
            hermetic_copy_size as usize,
            zx::VmarFlags::PERM_READ_IF_XOM_UNSUPPORTED | zx::VmarFlags::PERM_EXECUTE,
        )?;

        let (tx, rx) = std::sync::mpsc::channel::<zx::Status>();

        let shutdown_event = zx::Event::create();
        let shutdown_event_clone =
            shutdown_event.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();

        let join_handle = std::thread::spawn(move || {
            let exception_channel_result =
                fuchsia_runtime::job_default().create_exception_channel();

            let exception_channel = match exception_channel_result {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(e);
                    return;
                }
            };

            // register exception handler
            let _ = tx.send(zx::Status::OK);

            // loop on exceptions
            loop {
                let mut wait_items = [
                    zx::WaitItem {
                        handle: exception_channel.as_handle_ref(),
                        waitfor: zx::Signals::CHANNEL_READABLE,
                        pending: zx::Signals::empty(),
                    },
                    zx::WaitItem {
                        handle: shutdown_event_clone.as_handle_ref(),
                        waitfor: zx::Signals::USER_0,
                        pending: zx::Signals::empty(),
                    },
                ];
                let _ = zx::object_wait_many(&mut wait_items, zx::Time::INFINITE);
                if wait_items[1].pending == zx::Signals::USER_0 {
                    break;
                }
                let mut buf = zx::MessageBuf::new();
                exception_channel.read(&mut buf).unwrap();

                let excp_info = zx::sys::zx_exception_info_t::read_from(buf.bytes()).unwrap();

                if excp_info.type_ != zx::sys::ZX_EXCP_FATAL_PAGE_FAULT {
                    // Only process page faults.
                    continue;
                }

                let excp = zx::Exception::from_handle(buf.take_handle(0).unwrap());

                #[cfg(target_arch = "x86_64")]
                {
                    let thread = excp.get_thread().unwrap();
                    let mut regs = thread.read_state_general_regs().unwrap();

                    let pc = regs.rip as usize;
                    if !(hermetic_copy_mapped_addr
                        ..hermetic_copy_mapped_addr + hermetic_copy_size as usize)
                        .contains(&pc)
                    {
                        continue;
                    }

                    let report = thread.get_exception_report().unwrap();

                    let fault_address = unsafe { report.context.arch.x86_64.cr2 };
                    if !restricted_address_range.contains(&(fault_address as usize)) {
                        continue;
                    }

                    regs.rip = hermetic_copy_error as u64;
                    regs.rax = fault_address;
                    thread.write_state_general_regs(regs).unwrap();
                }

                // To avoid unused errors.
                #[cfg(not(target_arch = "x86_64"))]
                let _ = restricted_address_range;

                excp.set_exception_state(&zx::sys::ZX_EXCEPTION_STATE_HANDLED).unwrap();
            }
        });

        match rx.recv().unwrap() {
            zx::Status::OK => {}
            s => {
                return Err(s);
            }
        };

        let ptr = hermetic_copy_mapped_addr as *const ();
        let hermetic_copy_fn: HermeticCopyFn = unsafe { std::mem::transmute(ptr) };
        Ok(Some(Self { hermetic_copy_fn, shutdown_event, join_handle: Some(join_handle) }))
    }

    // Copies data from |source| to the restricted address |dest_addr|.
    // Returns Ok(()) if all bytes were copied without an error.
    // Returns Err(num_bytes) with the number of bytes copied if a fault was encountered.
    pub fn copyout(&self, source: &[u8], dest_addr: usize) -> Result<(), usize> {
        // Assumption: The address 0 is invalid and cannot be mapped.  The error encoding scheme has
        // a collision on the value 0 - it could mean that there was a fault at the address 0 or
        // that there was no fault. We want to treat an attempt to copy to 0 as a fault always.
        if dest_addr == 0 {
            return Err(0);
        }
        let ret = unsafe {
            (self.hermetic_copy_fn)(
                dest_addr as *mut std::ffi::c_void,
                source.as_ptr() as *const std::ffi::c_void,
                source.len(),
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            let fault_address = ret;
            Err(fault_address - dest_addr)
        }
    }

    // Copies data from the restricted address |source_addr| to |dest|
    // Returns Ok(()) if all bytes were copied without an error.
    // Returns Err(num_bytes) with the number of bytes copied if a fault was encountered.
    pub fn copyin(&self, source_addr: usize, dest: &mut [u8]) -> Result<(), usize> {
        // Assumption: The address 0 is invalid and cannot be mapped.  The error encoding scheme has
        // a collision on the value 0 - it could mean that there was a fault at the address 0 or
        // that there was no fault. We want to treat an attempt to copy from 0 as a fault always.
        if source_addr == 0 {
            return Err(0);
        }
        let ret = unsafe {
            (self.hermetic_copy_fn)(
                dest.as_ptr() as *mut std::ffi::c_void,
                source_addr as *const std::ffi::c_void,
                dest.len(),
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            let fault_address = ret;
            Err(fault_address - source_addr)
        }
    }
}

impl Drop for Usercopy {
    fn drop(&mut self) {
        self.shutdown_event.signal_handle(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        self.join_handle.take().unwrap().join().unwrap();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    macro_rules! new_usercopy_for_test {
        ($stmt:expr) => {{
            let usercopy = $stmt.unwrap();
            if cfg!(target_arch = "x86_64") {
                usercopy.unwrap()
            } else {
                // Skip test if not a supported architecture
                assert!(usercopy.is_none());
                return;
            }
        }};
    }

    #[::fuchsia::test]
    fn copyout_no_fault() {
        let page_size = zx::system_get_page_size() as usize;

        let source = vec!['a' as u8; 128];

        let dest_vmo = zx::Vmo::create(page_size as u64).unwrap();

        let root_vmar = fuchsia_runtime::vmar_root_self();

        let mapped_addr = root_vmar
            .map(0, &dest_vmo, 0, page_size, zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE)
            .unwrap();

        let usercopy = new_usercopy_for_test!(Usercopy::new(mapped_addr..mapped_addr + page_size));

        let result = usercopy.copyout(&source, mapped_addr);

        assert_eq!(result, Ok(()));

        assert_eq!(
            unsafe { std::slice::from_raw_parts(mapped_addr as *const u8, 128) },
            &['a' as u8; 128]
        );
    }

    #[::fuchsia::test]
    fn copyout_fault() {
        let page_size = zx::system_get_page_size() as usize;

        let source = vec!['a' as u8; 128];

        let dest_vmo = zx::Vmo::create(page_size as u64).unwrap();

        let root_vmar = fuchsia_runtime::vmar_root_self();

        let mapped_addr = root_vmar
            .map(
                0,
                &dest_vmo,
                0,
                page_size * 2,
                zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
            )
            .unwrap();

        let usercopy =
            new_usercopy_for_test!(Usercopy::new(mapped_addr..mapped_addr + page_size * 2));

        let dest_addr = mapped_addr + page_size - 32;

        let result = usercopy.copyout(&source, dest_addr);

        assert_eq!(result, Err(32));

        assert_eq!(
            unsafe { std::slice::from_raw_parts(dest_addr as *const u8, 32) },
            &['a' as u8; 32]
        );
    }

    #[::fuchsia::test]
    fn copyin_no_fault() {
        let page_size = zx::system_get_page_size() as usize;

        let mut dest = vec![0u8; 128];

        let source_vmo = zx::Vmo::create(page_size as u64).unwrap();

        let root_vmar = fuchsia_runtime::vmar_root_self();

        let mapped_addr = root_vmar
            .map(0, &source_vmo, 0, page_size, zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE)
            .unwrap();

        unsafe { std::slice::from_raw_parts_mut(mapped_addr as *mut u8, 128) }.fill('a' as u8);

        let usercopy = new_usercopy_for_test!(Usercopy::new(mapped_addr..mapped_addr + page_size));

        let result = usercopy.copyin(mapped_addr, &mut dest);

        assert_eq!(result, Ok(()));

        assert_eq!(dest, &['a' as u8; 128]);
    }

    #[::fuchsia::test]
    fn copyin_fault() {
        let page_size = zx::system_get_page_size() as usize;

        let mut dest = vec![0u8; 128];

        let source_vmo = zx::Vmo::create(page_size as u64).unwrap();

        let root_vmar = fuchsia_runtime::vmar_root_self();

        let mapped_addr = root_vmar
            .map(
                0,
                &source_vmo,
                0,
                page_size * 2,
                zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
            )
            .unwrap();

        let source_addr = mapped_addr + page_size - 32;

        unsafe { std::slice::from_raw_parts_mut(source_addr as *mut u8, 32) }.fill('a' as u8);

        let usercopy =
            new_usercopy_for_test!(Usercopy::new(mapped_addr..mapped_addr + page_size * 2));

        let result = usercopy.copyin(source_addr, &mut dest);

        assert_eq!(result, Err(32));

        assert_eq!(&dest[0..32], &['a' as u8; 32]);
        assert_eq!(&dest[32..128], &[0 as u8; 96]);
    }

    #[::fuchsia::test]
    fn fault_on_zero_address_copyin() {
        let usercopy = new_usercopy_for_test!(Usercopy::new(0..1));

        let mut dest = vec![0u8];

        let result = usercopy.copyin(0, &mut dest);

        assert_eq!(result, Err(0));
    }

    #[::fuchsia::test]
    fn fault_on_zero_address_copyout() {
        let usercopy = new_usercopy_for_test!(Usercopy::new(0..1));

        let source = vec![0u8];

        let result = usercopy.copyout(&source, 0);

        assert_eq!(result, Err(0));
    }
}
