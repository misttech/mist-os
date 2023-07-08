// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_IO_BUFFER_DISPATCHER_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_IO_BUFFER_DISPATCHER_H_

#include <zircon/rights.h>
#include <zircon/syscalls/iob.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>

#include <fbl/inline_array.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <object/dispatcher.h>
#include <object/handle.h>
#include <vm/vm_object.h>

enum class IobEndpointId : size_t { Ep0 = 0, Ep1 = 1 };

class IoBufferDispatcher final : public PeeredDispatcher<IoBufferDispatcher, ZX_DEFAULT_IOB_RIGHTS>,
                                 VmObjectChildObserver {
 public:
  // Make sure that RegionArray is small enough to comfortably fit on the stack.
  using RegionArray = fbl::InlineArray<zx_iob_region_t, 4>;
  static_assert(sizeof(RegionArray) < 500);

  /// Create a pair of IoBufferDispatchers
  static zx_status_t Create(uint64_t options, const RegionArray& region_configs,
                            const fbl::RefPtr<AttributionObject>& attribution_object,
                            KernelHandle<IoBufferDispatcher>* handle0,
                            KernelHandle<IoBufferDispatcher>* handle1, zx_rights_t* rights);

  ~IoBufferDispatcher() final;
  zx_obj_type_t get_type() const final { return ZX_OBJ_TYPE_IOB; }

  IobEndpointId GetEndpointId() const { return endpoint_id_; }
  zx_rights_t GetMapRights(zx_rights_t iob_rights, size_t region_index) const;
  zx_rights_t GetMediatedRights(size_t region_index) const;
  const fbl::RefPtr<VmObject>& GetVmo(size_t region_index) const;
  zx_iob_region_t GetRegion(size_t region_index) const;
  size_t RegionCount() const;

  // PeeredDispatcher implementation
  void on_zero_handles_locked() TA_REQ(get_lock());
  void OnPeerZeroHandlesLocked() TA_REQ(get_lock());

  // VmObjectChildObserver implementation
  void OnZeroChild() override;
  void OnOneChild() override;

 private:
  class IobRegion {
   public:
    IobRegion() = default;
    IobRegion(fbl::RefPtr<VmObject> ep0_vmo, fbl::RefPtr<VmObject> ep1_vmo, zx_iob_region_t region)
        : ep_vmos_{ktl::move(ep0_vmo), ktl::move(ep1_vmo)}, region_(region) {}

    zx_rights_t GetMapRights(IobEndpointId id) const {
      zx_rights_t rights = 0;
      switch (id) {
        case IobEndpointId::Ep0:
          if (region_.access & ZX_IOB_EP0_CAN_MAP_WRITE) {
            rights |= ZX_RIGHT_WRITE | ZX_RIGHT_MAP;
          }
          if (region_.access & ZX_IOB_EP0_CAN_MAP_READ) {
            rights |= ZX_RIGHT_READ | ZX_RIGHT_MAP;
          }
          break;
        case IobEndpointId::Ep1:
          if (region_.access & ZX_IOB_EP1_CAN_MAP_WRITE) {
            rights |= ZX_RIGHT_WRITE | ZX_RIGHT_MAP;
          }
          if (region_.access & ZX_IOB_EP1_CAN_MAP_READ) {
            rights |= ZX_RIGHT_READ | ZX_RIGHT_MAP;
          }
          break;
      }
      return rights;
    }

    const fbl::RefPtr<VmObject>& GetVmo(IobEndpointId id) const {
      return ep_vmos_[static_cast<size_t>(id)];
    }

    zx_iob_region_t GetRegion() const { return region_; }

    zx_rights_t GetMediatedRights(IobEndpointId id) const {
      zx_rights_t rights = 0;
      switch (id) {
        case IobEndpointId::Ep0:
          if (region_.access & ZX_IOB_EP0_CAN_MEDIATED_WRITE) {
            rights |= ZX_RIGHT_WRITE;
          }
          if (region_.access & ZX_IOB_EP0_CAN_MEDIATED_READ) {
            rights |= ZX_RIGHT_READ;
          }
          break;
        case IobEndpointId::Ep1:
          if (region_.access & ZX_IOB_EP1_CAN_MEDIATED_WRITE) {
            rights |= ZX_RIGHT_WRITE;
          }
          if (region_.access & ZX_IOB_EP1_CAN_MEDIATED_READ) {
            rights |= ZX_RIGHT_READ;
          }
          break;
      }
      return rights;
    }

   private:
    // A child vmo reference for each endpoint so that they can individually be notified of maps and
    // unmaps.
    fbl::RefPtr<VmObject> ep_vmos_[2];
    zx_iob_region_t region_;
  };

  // Wrapper struct to allow both peers to hold a reference to the regions.
  struct SharedIobState : public fbl::RefCounted<SharedIobState> {
   public:
    fbl::Array<const IobRegion> regions;
  };

  explicit IoBufferDispatcher(fbl::RefPtr<PeerHolder<IoBufferDispatcher>> holder,
                              IobEndpointId endpoint_id,
                              fbl::RefPtr<const SharedIobState> shared_state);

  fbl::RefPtr<const SharedIobState> const shared_state_;
  const IobEndpointId endpoint_id_;
  size_t peer_mapped_regions_ TA_GUARDED(get_lock()) = 0;
  bool peer_zero_handles_ TA_GUARDED(get_lock()) = false;
};

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_IO_BUFFER_DISPATCHER_H_
