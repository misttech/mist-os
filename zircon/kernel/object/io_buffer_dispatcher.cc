// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <assert.h>
#include <lib/counters.h>
#include <string.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls/iob.h>
#include <zircon/types.h>

#include <cstdint>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/ref_ptr.h>
#include <kernel/attribution.h>
#include <kernel/lockdep.h>
#include <kernel/mutex.h>
#include <ktl/move.h>
#include <object/dispatcher.h>
#include <object/handle.h>
#include <object/io_buffer_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/pmm.h>
#include <vm/vm_object.h>

#define LOCAL_TRACE 0

KCOUNTER(dispatcher_iob_create_count, "dispatcher.iob.create")
KCOUNTER(dispatcher_iob_destroy_count, "dispatcher.iob.destroy")

// static
zx_status_t IoBufferDispatcher::Create(uint64_t options,
                                       const IoBufferDispatcher::RegionArray& region_configs,
                                       const fbl::RefPtr<AttributionObject>& attribution_object,
                                       KernelHandle<IoBufferDispatcher>* handle0,
                                       KernelHandle<IoBufferDispatcher>* handle1,
                                       zx_rights_t* rights) {
  if (region_configs.size() == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AllocChecker ac;
  auto holder0 = fbl::AdoptRef(new (&ac) PeerHolder<IoBufferDispatcher>());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  auto holder1 = holder0;

  fbl::RefPtr<SharedIobState> shared_regions = fbl::AdoptRef(new (&ac) SharedIobState{
      .regions = nullptr,
  });

  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  KernelHandle new_handle0(fbl::AdoptRef(
      new (&ac) IoBufferDispatcher(ktl::move(holder0), IobEndpointId::Ep0, shared_regions)));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  KernelHandle new_handle1(fbl::AdoptRef(
      new (&ac) IoBufferDispatcher(ktl::move(holder1), IobEndpointId::Ep1, shared_regions)));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  fbl::Array<IobRegion> region_inner = fbl::MakeArray<IobRegion>(&ac, region_configs.size());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  for (unsigned i = 0; i < region_configs.size(); i++) {
    zx_iob_region_t& region_config = region_configs[i];
    if (region_config.type != ZX_IOB_REGION_TYPE_PRIVATE) {
      return ZX_ERR_INVALID_ARGS;
    }

    // A note on resource management:
    //
    // Everything we allocate in this loop ultimately gets owned by the SharedIobState and will be
    // cleaned up when both PeeredDispatchers are destroyed and drop their reference to the
    // SharedIobState.
    //
    // However, there is a complication. The VmoChildObservers have a reference to the dispatchers
    // which creates a cycle: IoBufferDispatcher -> SharedIobState -> IobRegion -> VmObject ->
    // VmoChildObserver -> IoBufferDispatcher.
    //
    // Since the VmoChildObservers keep a raw pointers to the dispatchers, we need to be sure to
    // reset the the corresponding pointers when we destroy an IoBufferDispatcher. Otherwise when an
    // IoBufferDispatcher maps a region, it could try to update a destroyed peer.
    //
    // See: IoBufferDispatcher~IoBufferDispatcher.

    // We effectively duplicate the logic from sys_vmo_create here, but instead of creating a
    // kernel handle and dispatcher, we keep ownership of it and assign it to a region.

    uint32_t vmo_options = 0;
    zx_status_t status = VmObjectDispatcher::parse_create_syscall_flags(
        region_config.private_region.options, &vmo_options);
    if (status != ZX_OK) {
      return status;
    }

    fbl::RefPtr<VmObjectPaged> vmo;
    status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY | PMM_ALLOC_FLAG_CAN_WAIT, vmo_options,
                                   region_config.size, attribution_object, &vmo);
    if (status != ZX_OK) {
      return status;
    }

    // VmObjectPaged::Create will round up the size to the nearest page. We need to know the actual
    // size of the vmo to later return if asked.
    region_config.size = vmo->size();

    // In order to track mappings and unmappings separately for each endpoint, we give each
    // endpoint a child reference instead of the created vmo.
    fbl::RefPtr<VmObject> ep0_reference;
    fbl::RefPtr<VmObject> ep1_reference;

    Resizability resizability =
        vmo->is_resizable() ? Resizability::Resizable : Resizability::NonResizable;
    status = vmo->CreateChildReference(resizability, 0, 0, true, &ep0_reference);
    if (status != ZX_OK) {
      return status;
    }
    status = vmo->CreateChildReference(resizability, 0, 0, true, &ep1_reference);
    if (status != ZX_OK) {
      return status;
    }

    // Now each endpoint can observe the mappings created by the other
    ep0_reference->SetChildObserver(new_handle1.dispatcher().get());
    ep1_reference->SetChildObserver(new_handle0.dispatcher().get());

    region_inner[i] = IobRegion{ktl::move(ep0_reference), ktl::move(ep1_reference), region_config};
  }
  shared_regions->regions = std::move(region_inner);

  new_handle0.dispatcher()->InitPeer(new_handle1.dispatcher());
  new_handle1.dispatcher()->InitPeer(new_handle0.dispatcher());

  *rights = default_rights();
  *handle0 = ktl::move(new_handle0);
  *handle1 = ktl::move(new_handle1);

  return ZX_OK;
}

IoBufferDispatcher::IoBufferDispatcher(fbl::RefPtr<PeerHolder<IoBufferDispatcher>> holder,
                                       IobEndpointId endpoint_id,
                                       fbl::RefPtr<const SharedIobState> shared_state)
    : PeeredDispatcher(ktl::move(holder)),
      shared_state_(std::move(shared_state)),
      endpoint_id_(endpoint_id) {
  kcounter_add(dispatcher_iob_create_count, 1);
}

zx_rights_t IoBufferDispatcher::GetMapRights(zx_rights_t iob_rights, size_t region_index) const {
  DEBUG_ASSERT(region_index < shared_state_->regions.size());
  zx_rights_t region_rights = shared_state_->regions[region_index].GetMapRights(GetEndpointId());
  region_rights &= (iob_rights | ZX_RIGHT_MAP);
  return region_rights;
}

zx_rights_t IoBufferDispatcher::GetMediatedRights(size_t region_index) const {
  DEBUG_ASSERT(region_index < shared_state_->regions.size());
  return shared_state_->regions[region_index].GetMediatedRights(GetEndpointId());
}

const fbl::RefPtr<VmObject>& IoBufferDispatcher::GetVmo(size_t region_index) const {
  DEBUG_ASSERT(region_index < shared_state_->regions.size());
  return shared_state_->regions[region_index].GetVmo(GetEndpointId());
}

zx_iob_region_t IoBufferDispatcher::GetRegion(size_t region_index) const {
  DEBUG_ASSERT(region_index < shared_state_->regions.size());
  return shared_state_->regions[region_index].GetRegion();
}

size_t IoBufferDispatcher::RegionCount() const {
  canary_.Assert();
  return shared_state_->regions.size();
}

void IoBufferDispatcher::on_zero_handles_locked() { canary_.Assert(); }

void IoBufferDispatcher::OnPeerZeroHandlesLocked() {
  canary_.Assert();
  peer_zero_handles_ = true;
  if (peer_mapped_regions_ == 0) {
    UpdateStateLocked(0, ZX_IOB_PEER_CLOSED);
  }
}

void IoBufferDispatcher::OnZeroChild() {
  Guard<CriticalMutex> guard{get_lock()};
  DEBUG_ASSERT(peer_mapped_regions_ > 0);
  peer_mapped_regions_--;
  if (peer_mapped_regions_ == 0 && peer_zero_handles_) {
    UpdateStateLocked(0, ZX_IOB_PEER_CLOSED);
  }
}

void IoBufferDispatcher::OnOneChild() {
  Guard<CriticalMutex> guard{get_lock()};
  peer_mapped_regions_++;
}

IoBufferDispatcher::~IoBufferDispatcher() {
  // The other endpoint's vmos are set up to notify this endpoint when they map regions via a raw
  // pointer. Since we're about to be destroyed, we need to unregister.
  //
  // VMObject unregisters in when the last handle is closed, but we do it a bit differently here.
  //
  // If we unregister in OnPeerZeroHandlesLocked, then we update the ChildObserver, we create the
  // dependency get_lock() -> child_observer_lock_. Then when we OnZeroChild is called, we first get
  // the child_observer_lock_, then try to update the state, locking get_lock establishing
  // child_observer_lock_ -> get_lock() which could lead to some lock ordering issues.
  //
  // However, cleaning up in the destructor means that we could be notified while we are being
  // destroyed. This should be okay because:
  // - Each vmo's child observer is protected by its child_observer_lock_.
  // - While we are in this destructor, a notification may attempt to get a vmo's.
  //   child_observer_lock_.
  // - If we have already destroyed updated the observer, it will see a nullptr and not access our
  //   state.
  // - If we have not already destroyed the observer, it will notify us while continuing to hold the
  //   child observer lock.
  // - If we attempt to reset the observer that is notifying us, we will block until it is completed
  //   and releases the lock.
  // - Thus we cannot continue to the Destruction Sequence until there are no observers in progress
  //   accessing our state, and no observer can start accessing our state.
  IobEndpointId other_id =
      endpoint_id_ == IobEndpointId::Ep0 ? IobEndpointId::Ep1 : IobEndpointId::Ep0;
  for (const IobRegion& region : shared_state_->regions) {
    if (const fbl::RefPtr<VmObject>& vmo = region.GetVmo(other_id); vmo != nullptr) {
      vmo->SetChildObserver(nullptr);
    }
  }

  kcounter_add(dispatcher_iob_destroy_count, 1);
}
