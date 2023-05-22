// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_PROFILE_DISPATCHER_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_PROFILE_DISPATCHER_H_

#include <zircon/rights.h>
#include <zircon/syscalls/profile.h>
#include <zircon/types.h>

#include <object/dispatcher.h>
#include <object/handle.h>

class ProfileDispatcher final
    : public SoloDispatcher<ProfileDispatcher, ZX_DEFAULT_PROFILE_RIGHTS> {
 public:
  static zx_status_t Create(const zx_profile_info_t& info, KernelHandle<ProfileDispatcher>* handle,
                            zx_rights_t* rights);

  ~ProfileDispatcher() final;
  zx_obj_type_t get_type() const final { return ZX_OBJ_TYPE_PROFILE; }

  zx_status_t ApplyProfile(fbl::RefPtr<ThreadDispatcher> thread);
  zx_status_t ApplyProfile(fbl::RefPtr<VmAddressRegionDispatcher> vmar);

 private:
  explicit ProfileDispatcher(const ktl::optional<SchedulerState::BaseProfile>& profile,
                             const ktl::optional<cpu_mask_t>& cpu_mask,
                             const ktl::optional<VmAddressRegion::MemoryPriority>& memory_priority);

  const ktl::optional<SchedulerState::BaseProfile> profile_;
  const ktl::optional<cpu_mask_t> cpu_mask_;
  const ktl::optional<VmAddressRegion::MemoryPriority> memory_priority_;
};

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_PROFILE_DISPATCHER_H_
