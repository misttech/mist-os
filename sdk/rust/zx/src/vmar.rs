// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon vmar objects.

use crate::{
    object_get_info_single, object_get_info_vec, ok, sys, AsHandleRef, Handle, HandleBased,
    HandleRef, Koid, Name, ObjectQuery, Status, Topic, Vmo,
};
use bitflags::bitflags;

/// An object representing a Zircon
/// [virtual memory address region](https://fuchsia.dev/fuchsia-src/concepts/objects/vm_address_region.md).
///
/// As essentially a subtype of `Handle`, it can be freely interconverted.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Vmar(Handle);
impl_handle_based!(Vmar);

sys::zx_info_vmar_t!(VmarInfo);

impl From<sys::zx_info_vmar_t> for VmarInfo {
    fn from(sys::zx_info_vmar_t { base, len }: sys::zx_info_vmar_t) -> VmarInfo {
        VmarInfo { base, len }
    }
}

// VmarInfo is able to be safely replaced with a byte representation and is a PoD type.
unsafe impl ObjectQuery for VmarInfo {
    const TOPIC: Topic = Topic::VMAR;
    type InfoTy = VmarInfo;
}

struct VmarMapsInfo;
unsafe impl ObjectQuery for VmarMapsInfo {
    const TOPIC: Topic = Topic::VMAR_MAPS;
    type InfoTy = crate::sys::zx_info_maps_t;
}

/// Ergonomic wrapper around `zx_info_maps_t`.
#[derive(Copy, Clone)]
pub struct MapInfo {
    pub name: Name,
    pub base: usize,
    pub size: usize,
    pub depth: usize,
    pub details: MapDetails,
}

impl MapInfo {
    /// # Safety
    ///
    /// Must be passed a value written by the kernel.
    pub(crate) unsafe fn from_raw(
        sys::zx_info_maps_t { name, base, size, depth, r#type, u }: sys::zx_info_maps_t,
    ) -> Result<Self, Status> {
        let details = match r#type {
            sys::ZX_INFO_MAPS_TYPE_NONE => MapDetails::None,
            sys::ZX_INFO_MAPS_TYPE_ASPACE => MapDetails::AddressSpace,
            sys::ZX_INFO_MAPS_TYPE_VMAR => MapDetails::Vmar,
            sys::ZX_INFO_MAPS_TYPE_MAPPING => {
                // SAFETY: as long as this value was written by the kernel we can trust that the
                // type corresponds to this layout.
                let &sys::zx_info_maps_mapping_t {
                    mmu_flags,
                    vmo_koid,
                    vmo_offset,
                    committed_bytes,
                    populated_bytes,
                    committed_private_bytes,
                    populated_private_bytes,
                    committed_scaled_bytes,
                    populated_scaled_bytes,
                    committed_fractional_scaled_bytes,
                    populated_fractional_scaled_bytes,
                    ..
                } = unsafe { &u.mapping };
                MapDetails::Mapping(MappingDetails {
                    mmu_flags: VmarFlagsExtended::from_bits_retain(mmu_flags),
                    vmo_koid: Koid::from_raw(vmo_koid),
                    vmo_offset,
                    committed_bytes,
                    populated_bytes,
                    committed_private_bytes,
                    populated_private_bytes,
                    committed_scaled_bytes,
                    populated_scaled_bytes,
                    committed_fractional_scaled_bytes,
                    populated_fractional_scaled_bytes,
                })
            }
            _ => return Err(Status::INTERNAL),
        };
        Ok(Self { name: Name::from_raw(name), base, size, depth, details })
    }
}

impl std::fmt::Debug for MapInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapInfo")
            .field("name", &self.name)
            .field("base", &format_args!("{:#x}", self.base))
            .field("size", &self.size)
            .field("depth", &self.depth)
            .field("details", &self.details)
            .finish()
    }
}

#[derive(Copy, Clone, Debug)]
pub enum MapDetails {
    None,
    AddressSpace,
    Vmar,
    Mapping(MappingDetails),
}

impl MapDetails {
    pub fn as_mapping(&self) -> Option<MappingDetails> {
        match self {
            Self::Mapping(d) => Some(*d),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct MappingDetails {
    pub mmu_flags: VmarFlagsExtended,
    pub vmo_koid: Koid,
    pub vmo_offset: u64,
    pub committed_bytes: usize,
    pub populated_bytes: usize,
    pub committed_private_bytes: usize,
    pub populated_private_bytes: usize,
    pub committed_scaled_bytes: usize,
    pub populated_scaled_bytes: usize,
    pub committed_fractional_scaled_bytes: u64,
    pub populated_fractional_scaled_bytes: u64,
}

impl Vmar {
    pub fn allocate(
        &self,
        offset: usize,
        size: usize,
        flags: VmarFlags,
    ) -> Result<(Vmar, usize), Status> {
        let mut mapped = 0;
        let mut handle = 0;
        let status = unsafe {
            sys::zx_vmar_allocate(
                self.raw_handle(),
                flags.bits(),
                offset,
                size,
                &mut handle,
                &mut mapped,
            )
        };
        ok(status)?;
        unsafe { Ok((Vmar::from(Handle::from_raw(handle)), mapped)) }
    }

    pub fn map(
        &self,
        vmar_offset: usize,
        vmo: &Vmo,
        vmo_offset: u64,
        len: usize,
        flags: VmarFlags,
    ) -> Result<usize, Status> {
        let flags = VmarFlagsExtended::from_bits_truncate(flags.bits());
        unsafe { self.map_unsafe(vmar_offset, vmo, vmo_offset, len, flags) }
    }

    /// Directly call `zx_vmar_map`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because certain flags to `zx_vmar_map` may
    /// replace an existing mapping which is referenced elsewhere.
    pub unsafe fn map_unsafe(
        &self,
        vmar_offset: usize,
        vmo: &Vmo,
        vmo_offset: u64,
        len: usize,
        flags: VmarFlagsExtended,
    ) -> Result<usize, Status> {
        let mut mapped = 0;
        let status = sys::zx_vmar_map(
            self.0.raw_handle(),
            flags.bits(),
            vmar_offset,
            vmo.raw_handle(),
            vmo_offset,
            len,
            &mut mapped,
        );
        ok(status).map(|_| mapped)
    }

    /// Directly call `zx_vmar_unmap`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because unmapping memory regions can arbitrarily
    /// cause read, write, and execution errors. Among other things, the caller
    /// must ensure that:
    ///
    /// - The region being unmapped will not be accessed after unmapping.
    /// - All references to memory in the region must be dropped or forgotten
    ///   prior to calling this method.
    /// - If the region contained executable code, then code in the region must
    ///   not be currently executing and may not be executed in the future.
    ///
    /// This is not an exhaustive list, as there are many ways to cause memory
    /// unsafety with memory mappings.
    pub unsafe fn unmap(&self, addr: usize, len: usize) -> Result<(), Status> {
        // SAFETY: The caller has guaranteed that unmapping the given region
        // will not cause undefined behavior.
        ok(unsafe { sys::zx_vmar_unmap(self.0.raw_handle(), addr, len) })
    }

    /// Directly call `zx_vmar_protect`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because changing the access protections for
    /// memory regions can arbitrarily cause read, write, and execution errors.
    /// Among other things, the caller must ensure that if a read, write, or
    /// execute permission is removed from a memory region, it must not read,
    /// write, or execute it respetively.
    ///
    /// This is not an exhaustive list, as there are many ways to cause memory
    /// unsafety with memory mappings.
    pub unsafe fn protect(&self, addr: usize, len: usize, flags: VmarFlags) -> Result<(), Status> {
        // SAFETY: The caller has guaranteed that protecting the given region
        // will not cause undefined behavior.
        ok(unsafe { sys::zx_vmar_protect(self.raw_handle(), flags.bits(), addr, len) })
    }

    /// Directly call `zx_vmar_destroy`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because destroying a region unmaps all of the
    /// mappings within it. See [`Vmar::unmap`] for more details on how
    /// unmapping memory regions can cause memory unsafety.
    pub unsafe fn destroy(&self) -> Result<(), Status> {
        // SAFETY: The caller has guaranteed that destroying the given region
        // will not cause undefined behavior.
        ok(unsafe { sys::zx_vmar_destroy(self.raw_handle()) })
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_VMAR topic.
    pub fn info(&self) -> Result<VmarInfo, Status> {
        Ok(object_get_info_single::<VmarInfo>(self.as_handle_ref())?)
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_VMAR_MAPS topic.
    pub fn info_maps_vec(&self) -> Result<Vec<MapInfo>, Status> {
        object_get_info_vec::<VmarMapsInfo>(self.as_handle_ref())?
            .into_iter()
            .map(|i| {
                // SAFETY: these values were written by the kernel which is the requirement for this
                // function.
                unsafe { MapInfo::from_raw(i) }
            })
            .collect::<Result<Vec<_>, _>>()
    }
}

// TODO(smklein): Ideally we would have two separate sets of bitflags,
// and a union of both of them.
macro_rules! vmar_flags {
    (
        safe: [$($safe_name:ident : $safe_sys_name:ident,)*],
        extended: [$($ex_name:ident : $ex_sys_name:ident,)*],
    ) => {
        bitflags! {
            /// Flags to VMAR routines which are considered safe.
            #[repr(transparent)]
            #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub struct VmarFlags: sys::zx_vm_option_t {
                $(
                    const $safe_name = sys::$safe_sys_name;
                )*
            }
        }

        bitflags! {
            /// Flags to all VMAR routines.
            #[repr(transparent)]
            #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub struct VmarFlagsExtended: sys::zx_vm_option_t {
                $(
                    const $safe_name = sys::$safe_sys_name;
                )*
                $(
                    const $ex_name = sys::$ex_sys_name;
                )*
            }
        }
    };
}

vmar_flags! {
    safe: [
        PERM_READ: ZX_VM_PERM_READ,
        PERM_WRITE: ZX_VM_PERM_WRITE,
        PERM_EXECUTE: ZX_VM_PERM_EXECUTE,
        COMPACT: ZX_VM_COMPACT,
        SPECIFIC: ZX_VM_SPECIFIC,
        CAN_MAP_SPECIFIC: ZX_VM_CAN_MAP_SPECIFIC,
        CAN_MAP_READ: ZX_VM_CAN_MAP_READ,
        CAN_MAP_WRITE: ZX_VM_CAN_MAP_WRITE,
        CAN_MAP_EXECUTE: ZX_VM_CAN_MAP_EXECUTE,
        MAP_RANGE: ZX_VM_MAP_RANGE,
        REQUIRE_NON_RESIZABLE: ZX_VM_REQUIRE_NON_RESIZABLE,
        ALLOW_FAULTS: ZX_VM_ALLOW_FAULTS,
        OFFSET_IS_UPPER_LIMIT: ZX_VM_OFFSET_IS_UPPER_LIMIT,
        PERM_READ_IF_XOM_UNSUPPORTED: ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED,

        // Alignment options
        ALIGN_1KB: ZX_VM_ALIGN_1KB,
        ALIGN_2KB: ZX_VM_ALIGN_2KB,
        ALIGN_4KB: ZX_VM_ALIGN_4KB,
        ALIGN_8KB: ZX_VM_ALIGN_8KB,
        ALIGN_16KB: ZX_VM_ALIGN_16KB,
        ALIGN_32KB: ZX_VM_ALIGN_32KB,
        ALIGN_64KB: ZX_VM_ALIGN_64KB,
        ALIGN_128KB: ZX_VM_ALIGN_128KB,
        ALIGN_256KB: ZX_VM_ALIGN_256KB,
        ALIGN_512KB: ZX_VM_ALIGN_512KB,
        ALIGN_1MB: ZX_VM_ALIGN_1MB,
        ALIGN_2MB: ZX_VM_ALIGN_2MB,
        ALIGN_4MB: ZX_VM_ALIGN_4MB,
        ALIGN_8MB: ZX_VM_ALIGN_8MB,
        ALIGN_16MB: ZX_VM_ALIGN_16MB,
        ALIGN_32MB: ZX_VM_ALIGN_32MB,
        ALIGN_64MB: ZX_VM_ALIGN_64MB,
        ALIGN_128MB: ZX_VM_ALIGN_128MB,
        ALIGN_256MB: ZX_VM_ALIGN_256MB,
        ALIGN_512MB: ZX_VM_ALIGN_512MB,
        ALIGN_1GB: ZX_VM_ALIGN_1GB,
        ALIGN_2GB: ZX_VM_ALIGN_2GB,
        ALIGN_4GB: ZX_VM_ALIGN_4GB,
    ],
    extended: [
        SPECIFIC_OVERWRITE: ZX_VM_SPECIFIC_OVERWRITE,
    ],
}

#[cfg(test)]
mod tests {
    // The unit tests are built with a different crate name, but fuchsia_runtime returns a "real"
    // zx::Vmar that we need to use.
    use zx::{Status, VmarFlags};

    #[test]
    fn allocate_and_info() -> Result<(), Status> {
        let size = usize::pow(2, 20); // 1MiB
        let root_vmar = fuchsia_runtime::vmar_root_self();
        let (vmar, base) = root_vmar.allocate(0, size, VmarFlags::empty())?;

        let info = vmar.info()?;
        assert!(info.base == base);
        assert!(info.len == size);
        Ok(())
    }

    #[test]
    fn root_vmar_info() -> Result<(), Status> {
        let root_vmar = fuchsia_runtime::vmar_root_self();
        let info = root_vmar.info()?;
        assert!(info.base > 0);
        assert!(info.len > 0);
        Ok(())
    }

    #[test]
    fn root_vmar_maps() {
        let root_vmar = fuchsia_runtime::vmar_root_self();
        let info = root_vmar.info_maps_vec().unwrap();
        assert!(!info.is_empty());
    }
}
