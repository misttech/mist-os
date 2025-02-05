// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon iobuffer objects.

use crate::{ok, sys, AsHandleRef, Handle, HandleBased, HandleRef, Status};
use bitflags::bitflags;

// TODO(https://fxbug.dev/389788832): Point this at the right place when the documentation is fixed.
/// An object representing a Zircon
/// [IOBuffer](https://fuchsia.dev/reference/syscalls/iob_create).
///
/// As essentially a subtype of `Handle`, it can be freely interconverted.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Iob(Handle);
impl_handle_based!(Iob);

#[derive(Default)]
pub struct IobOptions;

#[derive(Clone, Copy)]
pub enum IobRegionType {
    Private { size: u64, options: IobRegionPrivateOptions },
}

impl IobRegionType {
    fn to_raw(&self) -> (sys::zx_iob_region_type_t, sys::zx_iob_region_extension_t) {
        match self {
            IobRegionType::Private { .. } => (
                sys::ZX_IOB_REGION_TYPE_PRIVATE,
                sys::zx_iob_region_extension_t { private_region: Default::default() },
            ),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct IobRegionPrivateOptions;

pub struct IobRegion {
    pub region_type: IobRegionType,
    pub access: IobAccess,
    pub discipline: IobDiscipline,
}

bitflags! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct IobAccess: u32 {
        const EP0_CAN_MAP_READ = sys::ZX_IOB_ACCESS_EP0_CAN_MAP_READ;
        const EP0_CAN_MAP_WRITE = sys::ZX_IOB_ACCESS_EP0_CAN_MAP_WRITE;
        const EP0_CAN_MEDIATED_READ = sys::ZX_IOB_ACCESS_EP0_CAN_MEDIATED_READ;
        const EP0_CAN_MEDIATED_WRITE = sys::ZX_IOB_ACCESS_EP0_CAN_MEDIATED_WRITE;
        const EP1_CAN_MAP_READ = sys::ZX_IOB_ACCESS_EP1_CAN_MAP_READ;
        const EP1_CAN_MAP_WRITE = sys::ZX_IOB_ACCESS_EP1_CAN_MAP_WRITE;
        const EP1_CAN_MEDIATED_READ = sys::ZX_IOB_ACCESS_EP1_CAN_MEDIATED_READ;
        const EP1_CAN_MEDIATED_WRITE = sys::ZX_IOB_ACCESS_EP1_CAN_MEDIATED_WRITE;
    }
}

#[derive(Clone, Copy)]
pub enum IobDiscipline {
    None,
}

impl IobDiscipline {
    fn to_raw(&self) -> sys::zx_iob_discipline_t {
        match self {
            IobDiscipline::None => sys::zx_iob_discipline_t {
                r#type: sys::ZX_IOB_DISCIPLINE_TYPE_NONE,
                ..Default::default()
            },
        }
    }
}

impl Iob {
    /// Creates an IOBuffer.
    ///
    /// Wraps the [zx_iob_create](https://fuchsia.dev/reference/syscalls/iob_create) syscall.
    pub fn create(_options: IobOptions, regions: &[IobRegion]) -> Result<(Iob, Iob), Status> {
        let raw_regions: Vec<_> = regions
            .iter()
            .map(|r| {
                let (r#type, extension) = r.region_type.to_raw();
                sys::zx_iob_region_t {
                    r#type,
                    access: r.access.bits(),
                    size: match &r.region_type {
                        IobRegionType::Private { size, .. } => *size,
                    },
                    discipline: r.discipline.to_raw(),
                    extension,
                }
            })
            .collect();
        let mut handle1 = 0;
        let mut handle2 = 0;
        let status = unsafe {
            sys::zx_iob_create(
                0,
                raw_regions.as_ptr() as *const u8,
                raw_regions.len(),
                &mut handle1,
                &mut handle2,
            )
        };
        ok(status)?;
        unsafe { Ok((Iob::from(Handle::from_raw(handle1)), Iob::from(Handle::from_raw(handle2)))) }
    }
}

#[cfg(test)]
mod tests {
    use super::{Iob, IobAccess, IobDiscipline, IobRegion, IobRegionType};
    use crate::{Unowned, Vmar, VmarFlags};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn test_create_iob() {
        let region_size = zx::system_get_page_size() as usize * 8;
        let (ep0, ep1) = Iob::create(
            Default::default(),
            &[IobRegion {
                region_type: IobRegionType::Private {
                    size: region_size as u64,
                    options: Default::default(),
                },
                access: IobAccess::EP0_CAN_MAP_READ
                    | IobAccess::EP0_CAN_MAP_WRITE
                    | IobAccess::EP1_CAN_MAP_READ,
                discipline: IobDiscipline::None,
            }],
        )
        .expect("create failed");

        // We can't use fuchsia_runtime other than to get the handle, because its Vmar type is
        // considered distinct from crate::Vmar.
        let root_vmar =
            unsafe { Unowned::<Vmar>::from_raw_handle(fuchsia_runtime::zx_vmar_root_self()) };

        let write_addr = root_vmar
            .map_iob(VmarFlags::PERM_READ | VmarFlags::PERM_WRITE, 0, &ep0, 0, 0, region_size)
            .expect("map_iob failed");
        let read_addr = root_vmar
            .map_iob(VmarFlags::PERM_READ, 0, &ep1, 0, 0, region_size)
            .expect("map_iob failed");

        const VALUE: u64 = 0x123456789abcdef;

        unsafe { &*(write_addr as *const AtomicU64) }.store(VALUE, Ordering::Relaxed);

        assert_eq!(unsafe { &*(read_addr as *const AtomicU64) }.load(Ordering::Relaxed), VALUE);

        unsafe {
            root_vmar.unmap(write_addr, region_size).expect("unmap failed");
            root_vmar.unmap(read_addr, region_size).expect("unmap failed");
        }
    }
}
