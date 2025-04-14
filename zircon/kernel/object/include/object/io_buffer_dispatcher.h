// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_IO_BUFFER_DISPATCHER_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_IO_BUFFER_DISPATCHER_H_

#include <lib/user_copy/user_ptr.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/rights.h>
#include <zircon/syscalls/iob.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>

#include <fbl/inline_array.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <ktl/byte.h>
#include <ktl/span.h>
#include <ktl/type_traits.h>
#include <ktl/variant.h>
#include <object/dispatcher.h>
#include <object/handle.h>
#include <object/io_buffer_shared_region_dispatcher.h>
#include <vm/pinned_vm_object.h>
#include <vm/vm_address_region.h>
#include <vm/vm_object.h>

enum class IobEndpointId : size_t { Ep0 = 0, Ep1 = 1 };

class IoBufferDispatcher : public PeeredDispatcher<IoBufferDispatcher, ZX_DEFAULT_IOB_RIGHTS>,
                           public VmObjectChildObserver {
 public:
  // Make sure that RegionArray is small enough to comfortably fit on the stack.
  using RegionArray = fbl::InlineArray<zx_iob_region_t, 4>;
  static_assert(sizeof(RegionArray) < 500);

  /// Create a pair of IoBufferDispatchers
  static zx_status_t Create(uint64_t options, const RegionArray& region_configs,
                            KernelHandle<IoBufferDispatcher>* handle0,
                            KernelHandle<IoBufferDispatcher>* handle1, zx_rights_t* rights);

  ~IoBufferDispatcher() override;
  zx_obj_type_t get_type() const final { return ZX_OBJ_TYPE_IOB; }

  IobEndpointId GetEndpointId() const { return endpoint_id_; }
  zx_rights_t GetMapRights(zx_rights_t iob_rights, size_t region_index) const;
  zx_rights_t GetMediatedRights(size_t region_index) const;
  const fbl::RefPtr<VmObject>& GetVmo(size_t region_index) const;
  zx::result<fbl::RefPtr<VmObject>> CreateMappableVmoForRegion(size_t region_index);
  zx_iob_region_t GetRegion(size_t region_index) const;
  size_t RegionCount() const;

  zx_info_iob_t GetInfo() const;
  zx_iob_region_info_t GetRegionInfo(size_t index) const;

  // Allocates an ID out of a ZX_IOB_DISCIPLINE_ID_ALLOCATOR region.
  zx::result<uint32_t> AllocateId(size_t region_index, user_in_ptr<const ktl::byte> blob_ptr,
                                  size_t blob_size);

  // Performs a mediated write to a region.
  zx::result<> Write(size_t region_index, user_in_iovec_t message);

  // PeeredDispatcher implementation
  void on_zero_handles_locked() TA_REQ(get_lock());
  virtual void OnPeerZeroHandlesLocked() TA_REQ(get_lock());
  zx_status_t set_name(const char* name, size_t len) override;
  zx_status_t get_name(char (&out_name)[ZX_MAX_NAME_LEN]) const override;

  // VmObjectChildObserver implementation
  void OnZeroChild() override;

 protected:
  friend class IoBufferWriterDispatcher;

  // The base IOB region type, presenting discipline-agnostic region interfaces
  // to the dispatcher. Discipline-specific interfaces are expected to be
  // defined by subclasses of this type (one per discipline type).
  class IobRegion {
   public:
    IobRegion() = default;

    IobRegion(fbl::RefPtr<VmObject> ep0_vmo,                 //
              fbl::RefPtr<VmObject> ep1_vmo,                 //
              fbl::RefPtr<VmMapping> mapping,                //
              zx_vaddr_t base,                               //
              const zx_iob_region_t& region,                 //
              zx_koid_t koid)                                //
        : ep_vmos_{ktl::move(ep0_vmo), ktl::move(ep1_vmo)},  //
          mapping_(ktl::move(mapping)),                      //
          base_(base),                                       //
          region_(region),                                   //
          koid_(koid) {
      ZX_DEBUG_ASSERT(ep_vmos_[0]);
      ZX_DEBUG_ASSERT(ep_vmos_[1]);
    }

    zx_iob_region_t region() const { return region_; }

    zx_rights_t GetMapRights(IobEndpointId id) const {
      zx_rights_t rights = 0;
      switch (id) {
        case IobEndpointId::Ep0:
          if (region_.access & ZX_IOB_ACCESS_EP0_CAN_MAP_WRITE) {
            rights |= ZX_RIGHT_WRITE | ZX_RIGHT_MAP;
          }
          if (region_.access & ZX_IOB_ACCESS_EP0_CAN_MAP_READ) {
            rights |= ZX_RIGHT_READ | ZX_RIGHT_MAP;
          }
          break;
        case IobEndpointId::Ep1:
          if (region_.access & ZX_IOB_ACCESS_EP1_CAN_MAP_WRITE) {
            rights |= ZX_RIGHT_WRITE | ZX_RIGHT_MAP;
          }
          if (region_.access & ZX_IOB_ACCESS_EP1_CAN_MAP_READ) {
            rights |= ZX_RIGHT_READ | ZX_RIGHT_MAP;
          }
          break;
      }
      return rights;
    }

    const fbl::RefPtr<VmObject>& GetVmo(IobEndpointId id) const {
      return ep_vmos_[static_cast<size_t>(id)];
    }

    zx_iob_region_info_t GetRegionInfo(bool swap_endpoints) const {
      zx_iob_region_info_t info{region_, koid_};
      if (swap_endpoints) {
        uint32_t ep0_access =
            info.region.access &
            (ZX_IOB_ACCESS_EP0_CAN_MAP_READ | ZX_IOB_ACCESS_EP0_CAN_MAP_WRITE |
             ZX_IOB_ACCESS_EP0_CAN_MEDIATED_READ | ZX_IOB_ACCESS_EP0_CAN_MEDIATED_WRITE);
        uint32_t ep1_access =
            info.region.access &
            (ZX_IOB_ACCESS_EP1_CAN_MAP_READ | ZX_IOB_ACCESS_EP1_CAN_MAP_WRITE |
             ZX_IOB_ACCESS_EP1_CAN_MEDIATED_READ | ZX_IOB_ACCESS_EP1_CAN_MEDIATED_WRITE);
        info.region.access = (ep0_access << 4) | (ep1_access >> 4);
      }
      return info;
    }

    zx_rights_t GetMediatedRights(IobEndpointId id) const {
      zx_rights_t rights = 0;
      switch (id) {
        case IobEndpointId::Ep0:
          if (region_.access & ZX_IOB_ACCESS_EP0_CAN_MEDIATED_WRITE) {
            rights |= ZX_RIGHT_WRITE;
          }
          if (region_.access & ZX_IOB_ACCESS_EP0_CAN_MEDIATED_READ) {
            rights |= ZX_RIGHT_READ;
          }
          break;
        case IobEndpointId::Ep1:
          if (region_.access & ZX_IOB_ACCESS_EP1_CAN_MEDIATED_WRITE) {
            rights |= ZX_RIGHT_WRITE;
          }
          if (region_.access & ZX_IOB_ACCESS_EP1_CAN_MEDIATED_READ) {
            rights |= ZX_RIGHT_READ;
          }
          break;
      }
      return rights;
    }

    // Debug asserts that the discipline is of the expected type, intended as a
    // guardrail for use in discipline-specific subclasses.
    void AssertDiscipline(zx_iob_discipline_type_t type) const {
      ZX_DEBUG_ASSERT(region_.discipline.type == type);
    }

   protected:
    zx_vaddr_t base() const { return base_; }

    fbl::RefPtr<VmMapping> mapping() { return mapping_; }

   private:
    // A child vmo reference for each endpoint so that they can individually be notified of maps
    // and unmaps.
    fbl::RefPtr<VmObject> ep_vmos_[2];

    // A mapping of the region into kernel space, or null if the region does
    // not permit mediated access.
    fbl::RefPtr<VmMapping> mapping_;

    // The base address of the mapping.
    zx_vaddr_t base_ = 0;

    zx_iob_region_t region_ = {};
    zx_koid_t koid_ = ZX_KOID_INVALID;
  };

  // Represents ZX_IOB_DISCIPLINE_TYPE_NONE.
  using IobRegionNone = IobRegion;

  // Represents ZX_IOB_DISCIPLINE_TYPE_ID_ALLOCATOR.
  class IobRegionIdAllocator : public IobRegion {
   public:
    using IobRegion::IobRegion;

    // When mediated access is requested, this initializes the ID allocator
    // container and pins and maps the whole region mapping for reliable future
    // access. This must be called before other methods, even in the
    // non-mediated cases.
    zx::result<> Init();

    // Returns a view into the whole kernel-mapped region.
    ktl::span<ktl::byte> mediated_bytes() {
      if (base() == 0) {
        return {};
      }
      return {
          reinterpret_cast<ktl::byte*>(base()),
          region().size,
      };
    }

    zx::result<uint32_t> AllocateId(IobEndpointId id, user_in_ptr<const ktl::byte> blob_ptr,
                                    size_t blob_size);

   private:
    PinnedVmObject pin_;
  };

  class IobRegionMediatedWriteRingBuffer : public IobRegion {
   public:
    IobRegionMediatedWriteRingBuffer(fbl::RefPtr<VmObject> ep0_vmo, fbl::RefPtr<VmObject> ep1_vmo,
                                     const zx_iob_region_t& region, zx_koid_t koid,
                                     fbl::RefPtr<IoBufferSharedRegionDispatcher> shared_region,
                                     uint64_t tag)
        : IobRegion(ep0_vmo, ep1_vmo, nullptr, 0, region, koid),
          shared_region_(std::move(shared_region)),
          tag_(tag) {}

    // Performs a mediated write to the region.
    zx::result<> Write(IobEndpointId id, user_in_iovec_t message);

   private:
    fbl::RefPtr<IoBufferSharedRegionDispatcher> shared_region_;
    uint64_t tag_;
  };

  using IobRegionVariant =
      ktl::variant<IobRegionNone, IobRegionIdAllocator, IobRegionMediatedWriteRingBuffer>;

  static zx::result<IobRegionVariant> CreateIobRegionVariant(
      fbl::RefPtr<VmObject> ep0_vmo,     //
      fbl::RefPtr<VmObject> ep1_vmo,     //
      fbl::RefPtr<VmObject> kernel_vmo,  //
      const zx_iob_region_t& region,     //
      zx_koid_t koid,                    //
      fbl::RefPtr<IoBufferSharedRegionDispatcher> shared_region);

  // Wrapper struct to allow both peers to hold a reference to the regions.
  struct SharedIobState : public fbl::RefCounted<SharedIobState> {
   public:
    // Reinterprets the `i`-th region as particular region type. The default
    // parameter `IobRegion` is always valid, and may be used to access a given
    // regions's discipline-agnostic interface.
    template <typename T = const IobRegion,
              typename = ktl::enable_if_t<ktl::is_base_of_v<IobRegion, ktl::remove_const_t<T>>>>
    T* GetRegionLocked(size_t region_index) TA_REQ(state_lock) {
      ZX_DEBUG_ASSERT(region_index < regions.size());
      if constexpr (ktl::is_same_v<ktl::remove_const_t<T>, IobRegion>) {
        return ktl::visit([](IobRegion& region) { return &region; }, regions[region_index]);
      }
      return ktl::get_if<ktl::remove_const_t<T>>(&regions[region_index]);
    }

    template <typename T = const IobRegion,
              typename = ktl::enable_if_t<ktl::is_base_of_v<IobRegion, ktl::remove_const_t<T>>>>
    T* GetRegion(size_t region_index) {
      Guard<CriticalMutex> guard{&state_lock};
      return GetRegionLocked<T>(region_index);
    }

    DECLARE_CRITICAL_MUTEX(SharedIobState) state_lock;
    fbl::Array<IobRegionVariant> TA_GUARDED(state_lock) regions;
  };

  static zx::result<fbl::Array<IobRegionVariant>> CreateRegions(
      const IoBufferDispatcher::RegionArray& region_configs, VmObjectChildObserver* ep0,
      VmObjectChildObserver* ep1);

  explicit IoBufferDispatcher(fbl::RefPtr<PeerHolder<IoBufferDispatcher>> holder,
                              IobEndpointId endpoint_id, fbl::RefPtr<SharedIobState> shared_state);

 private:
  fbl::RefPtr<SharedIobState> const shared_state_;
  const IobEndpointId endpoint_id_;

  size_t peer_mapped_regions_ TA_GUARDED(get_lock()) = 0;
  bool peer_zero_handles_ TA_GUARDED(get_lock()) = false;
};

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_IO_BUFFER_DISPATCHER_H_
