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
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <cstdint>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/ref_ptr.h>
#include <kernel/koid.h>
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
zx::result<fbl::Array<IoBufferDispatcher::IobRegion>> IoBufferDispatcher::CreateRegions(
    const IoBufferDispatcher::RegionArray& region_configs, VmObjectChildObserver* ep0,
    VmObjectChildObserver* ep1) {
  fbl::AllocChecker ac;
  fbl::Array<IobRegion> region_inner = fbl::MakeArray<IobRegion>(&ac, region_configs.size());
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  for (unsigned i = 0; i < region_configs.size(); i++) {
    zx_iob_region_t region_config = region_configs[i];
    if (region_config.type != ZX_IOB_REGION_TYPE_PRIVATE) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    if (region_config.access == 0) {
      return zx::error(ZX_ERR_INVALID_ARGS);
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

    zx::result<VmObjectDispatcher::CreateStats> parse_result =
        VmObjectDispatcher::parse_create_syscall_flags(region_config.private_region.options,
                                                       region_config.size);
    if (parse_result.is_error()) {
      return zx::error(parse_result.error_value());
    }
    VmObjectDispatcher::CreateStats stats = parse_result.value();

    fbl::RefPtr<VmObjectPaged> vmo;
    if (zx_status_t status =
            VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, stats.flags, stats.size, &vmo);
        status != ZX_OK) {
      return zx::error(status);
    }

    // VmObjectPaged::Create will round up the size to the nearest page, or set the size to the
    // maximum possible VMO size if ZX_VMO_UNBOUNDED is used. We need to know the actual size of the
    // vmo to later return if asked.
    region_config.size = vmo->size();
    zx_koid_t koid = KernelObjectId::Generate();
    vmo->set_user_id(koid);

    // In order to track mappings and unmappings separately for each endpoint, we give each
    // endpoint a child reference instead of the created vmo.
    fbl::RefPtr<VmObject> ep0_reference;
    fbl::RefPtr<VmObject> ep1_reference;

    Resizability resizability =
        vmo->is_resizable() ? Resizability::Resizable : Resizability::NonResizable;
    if (zx_status_t status =
            vmo->CreateChildReference(resizability, 0, 0, true, nullptr, &ep0_reference);
        status != ZX_OK) {
      return zx::error(status);
    }
    if (zx_status_t status =
            vmo->CreateChildReference(resizability, 0, 0, true, nullptr, &ep1_reference);
        status != ZX_OK) {
      return zx::error(status);
    }

    ep0_reference->set_user_id(koid);
    ep1_reference->set_user_id(koid);

    // Now each endpoint can observe the mappings created by the other
    ep0_reference->SetChildObserver(ep1);
    ep1_reference->SetChildObserver(ep0);

    region_inner[i] =
        IobRegion{ktl::move(ep0_reference), ktl::move(ep1_reference), region_config, koid};
  }
  return zx::ok(ktl::move(region_inner));
}

// static
zx_status_t IoBufferDispatcher::Create(uint64_t options,
                                       const IoBufferDispatcher::RegionArray& region_configs,
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

  zx::result<fbl::Array<IobRegion>> regions =
      CreateRegions(region_configs, new_handle0.dispatcher().get(), new_handle1.dispatcher().get());
  if (regions.is_error()) {
    return regions.error_value();
  }
  {
    Guard<CriticalMutex> guard{&shared_regions->state_lock};
    shared_regions->regions = *ktl::move(regions);
  }

  new_handle0.dispatcher()->InitPeer(new_handle1.dispatcher());
  new_handle1.dispatcher()->InitPeer(new_handle0.dispatcher());

  *rights = default_rights();
  *handle0 = ktl::move(new_handle0);
  *handle1 = ktl::move(new_handle1);

  return ZX_OK;
}

IoBufferDispatcher::IoBufferDispatcher(fbl::RefPtr<PeerHolder<IoBufferDispatcher>> holder,
                                       IobEndpointId endpoint_id,
                                       fbl::RefPtr<SharedIobState> shared_state)
    : PeeredDispatcher(ktl::move(holder)),
      shared_state_(ktl::move(shared_state)),
      endpoint_id_(endpoint_id) {
  kcounter_add(dispatcher_iob_create_count, 1);
}

zx_rights_t IoBufferDispatcher::GetMapRights(zx_rights_t iob_rights, size_t region_index) const {
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
  DEBUG_ASSERT(region_index < shared_state_->regions.size());
  zx_rights_t region_rights = shared_state_->regions[region_index].GetMapRights(GetEndpointId());
  region_rights &= (iob_rights | ZX_RIGHT_MAP);
  return region_rights;
}

zx_rights_t IoBufferDispatcher::GetMediatedRights(size_t region_index) const {
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
  DEBUG_ASSERT(region_index < shared_state_->regions.size());
  return shared_state_->regions[region_index].GetMediatedRights(GetEndpointId());
}

const fbl::RefPtr<VmObject>& IoBufferDispatcher::GetVmo(size_t region_index) const {
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
  DEBUG_ASSERT(region_index < shared_state_->regions.size());
  return shared_state_->regions[region_index].GetVmo(GetEndpointId());
}

zx_iob_region_t IoBufferDispatcher::GetRegion(size_t region_index) const {
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
  DEBUG_ASSERT(region_index < shared_state_->regions.size());
  return shared_state_->regions[region_index].GetRegion();
}

size_t IoBufferDispatcher::RegionCount() const {
  canary_.Assert();
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
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

  // OnZeroChildren gets called every time the number of children is equal to zero. Due to races, by
  // the time this method is called we could already have children again. This is fine, since all
  // we need to do is ensure that every increment of peer_mapped_regions_ is paired with a
  // decrement.
  DEBUG_ASSERT(peer_mapped_regions_ > 0);
  peer_mapped_regions_--;
  if (peer_mapped_regions_ == 0 && peer_zero_handles_) {
    UpdateStateLocked(0, ZX_IOB_PEER_CLOSED);
  }
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
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
  IobEndpointId other_id =
      endpoint_id_ == IobEndpointId::Ep0 ? IobEndpointId::Ep1 : IobEndpointId::Ep0;
  for (const IobRegion& region : shared_state_->regions) {
    if (const fbl::RefPtr<VmObject>& vmo = region.GetVmo(other_id); vmo != nullptr) {
      vmo->SetChildObserver(nullptr);
    }
  }

  kcounter_add(dispatcher_iob_destroy_count, 1);
}

zx_info_iob_t IoBufferDispatcher::GetInfo() const {
  canary_.Assert();
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
  return {.options = 0, .region_count = static_cast<uint32_t>(shared_state_->regions.size())};
}

zx_iob_region_info_t IoBufferDispatcher::GetRegionInfo(size_t index) const {
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
  DEBUG_ASSERT(index < shared_state_->regions.size());
  canary_.Assert();
  return shared_state_->regions[index].GetRegionInfo(endpoint_id_ == IobEndpointId::Ep1);
}

zx_status_t IoBufferDispatcher::get_name(char (&out_name)[ZX_MAX_NAME_LEN]) const {
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
  shared_state_->regions[0].GetVmo(IobEndpointId::Ep0)->get_name(out_name, ZX_MAX_NAME_LEN);
  return ZX_OK;
}

zx_status_t IoBufferDispatcher::set_name(const char* name, size_t len) {
  canary_.Assert();
  Guard<CriticalMutex> guard{&shared_state_->state_lock};
  for (const IobRegion& region : shared_state_->regions) {
    switch (region.GetRegion().type) {
      case ZX_IOB_REGION_TYPE_PRIVATE: {
        zx_status_t region_result = region.GetVmo(IobEndpointId::Ep0)->set_name(name, len);
        if (region_result != ZX_OK) {
          return region_result;
        }
        region_result = region.GetVmo(IobEndpointId::Ep1)->set_name(name, len);
        if (region_result != ZX_OK) {
          return region_result;
        }
        break;
      }
      default:
        continue;
    }
  }
  return ZX_OK;
}

zx::result<fbl::RefPtr<VmObject>> IoBufferDispatcher::CreateMappableVmoForRegion(
    size_t region_index) {
  fbl::RefPtr<VmObject> child_reference;
  const fbl::RefPtr<VmObject>& vmo = GetVmo(region_index);
  Resizability resizability =
      vmo->is_resizable() ? Resizability::Resizable : Resizability::NonResizable;
  {
    bool first_child = false;

    zx_status_t status =
        vmo->CreateChildReference(resizability, 0, 0, true, &first_child, &child_reference);
    if (status != ZX_OK) {
      return zx::error(status);
    }
    if (first_child) {
      // Need to record 0->1 transition. As we are holding a refptr to the child we know that
      // OnZeroChildren cannot get called to decrement before we can increment. In this case the
      // lock is not attempting to synchronize anything beyond the raw access of
      // peer_mapped_regions_.
      Guard<CriticalMutex> guard{get_lock()};
      const fbl::RefPtr<IoBufferDispatcher>& p = peer();
      AssertHeld(*p->get_lock());
      p->peer_mapped_regions_++;
    }
  }
  child_reference->set_user_id(vmo->user_id());
  return zx::ok(child_reference);
}
