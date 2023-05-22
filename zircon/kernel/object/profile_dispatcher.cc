// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "object/profile_dispatcher.h"

#include <bits.h>
#include <lib/counters.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <ktl/bit.h>
#include <object/thread_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>

#include <ktl/enforce.h>

KCOUNTER(dispatcher_profile_create_count, "dispatcher.profile.create")
KCOUNTER(dispatcher_profile_destroy_count, "dispatcher.profile.destroy")

static zx::result<cpu_mask_t> parse_cpu_mask(const zx_cpu_set_t& set) {
  // The code below only supports reading up to 1 word in the mask.
  static_assert(SMP_MAX_CPUS <= sizeof(set.mask[0]) * CHAR_BIT);
  static_assert(SMP_MAX_CPUS <= sizeof(cpu_mask_t) * CHAR_BIT);
  static_assert(SMP_MAX_CPUS <= ZX_CPU_SET_MAX_CPUS);

  // We throw away any bits beyond SMP_MAX_CPUs.
  return zx::ok(static_cast<cpu_mask_t>(set.mask[0] & BIT_MASK(SMP_MAX_CPUS)));
}

static zx::result<SchedulerState::BaseProfile> validate_and_create_profile(
    const zx_profile_info_t& info) {
  // Ensure that none of the flags outside of the set of valid flags has been set.
  constexpr uint32_t kAllFlags = (ZX_PROFILE_INFO_FLAG_PRIORITY | ZX_PROFILE_INFO_FLAG_CPU_MASK |
                                  ZX_PROFILE_INFO_FLAG_DEADLINE | ZX_PROFILE_INFO_FLAG_NO_INHERIT);
  if ((info.flags & ~kAllFlags) != 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Ensure that exactly one of the "discipline selection" flags has been set.
  constexpr uint32_t kDisciplineSelectionFlags =
      ZX_PROFILE_INFO_FLAG_PRIORITY | ZX_PROFILE_INFO_FLAG_DEADLINE;
  if (ktl::popcount(info.flags & kDisciplineSelectionFlags) != 1) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Deadline profiles may not be flagged as NO_INHERIT
  constexpr uint32_t kDeadlineNoInherit =
      ZX_PROFILE_INFO_FLAG_DEADLINE | ZX_PROFILE_INFO_FLAG_NO_INHERIT;
  if ((info.flags & kDeadlineNoInherit) == kDeadlineNoInherit) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // If selected, ensure priority is valid.
  if ((info.flags & ZX_PROFILE_INFO_FLAG_PRIORITY) != 0) {
    if ((info.priority < LOWEST_PRIORITY) || (info.priority > HIGHEST_PRIORITY)) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  // If selected, ensure the deadline parameters are valid.  Note that deadline
  // profiles must currently be inheritable.
  const bool inheritable = (info.flags & ZX_PROFILE_INFO_FLAG_NO_INHERIT) == 0;
  if ((info.flags & ZX_PROFILE_INFO_FLAG_DEADLINE) != 0) {
    // TODO(eieio): Add additional admission criteria to prevent values that are
    // too large or too small. These values are mediated by a privileged service
    // so the risk of abuse is low, but it still might be good to implement some
    // sort of failsafe check to prevent mistakes.
    const bool admissible =
        info.deadline_params.capacity > 0 &&
        info.deadline_params.capacity <= info.deadline_params.relative_deadline &&
        info.deadline_params.relative_deadline <= info.deadline_params.period && inheritable;
    if (!admissible) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  if (info.flags & ZX_PROFILE_INFO_FLAG_PRIORITY) {
    return zx::ok(SchedulerState::BaseProfile(info.priority, inheritable));
  } else {
    DEBUG_ASSERT(inheritable == true);
    return zx::ok(SchedulerState::BaseProfile(info.deadline_params));
  }
}

static zx::result<VmAddressRegion::MemoryPriority> parse_memory_priority(
    const zx_profile_info_t& info) {
  if (info.priority == ZX_PRIORITY_HIGH) {
    return zx::ok(VmAddressRegion::MemoryPriority::HIGH);
  }
  if (info.priority == ZX_PRIORITY_DEFAULT) {
    return zx::ok(VmAddressRegion::MemoryPriority::DEFAULT);
  }
  return zx::error(ZX_ERR_INVALID_ARGS);
}

zx_status_t ProfileDispatcher::Create(const zx_profile_info_t& info,
                                      KernelHandle<ProfileDispatcher>* handle,
                                      zx_rights_t* rights) {
  // A profile must specify at least a set of scheduling parameters, or a cpu
  // affinity mask, or a memory priority.
  constexpr uint32_t kSchedFlags = ZX_PROFILE_INFO_FLAG_PRIORITY | ZX_PROFILE_INFO_FLAG_DEADLINE;
  constexpr uint32_t kAffinityFlags = ZX_PROFILE_INFO_FLAG_CPU_MASK;
  constexpr uint32_t kThreadFlags = kSchedFlags | kAffinityFlags;
  constexpr uint32_t kMemoryFlags = ZX_PROFILE_INFO_FLAG_MEMORY_PRIORITY;
  constexpr uint32_t kRequiredFlags = kThreadFlags | kMemoryFlags;

  if ((info.flags & kRequiredFlags) == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Memory flags are incompatible with thread related flags.
  if ((info.flags & kMemoryFlags) != 0 && (info.flags & kThreadFlags) != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  ktl::optional<SchedulerState::BaseProfile> profile;
  if ((info.flags & kSchedFlags) != 0) {
    zx::result<SchedulerState::BaseProfile> maybe_profile = validate_and_create_profile(info);

    if (maybe_profile.is_error()) {
      return maybe_profile.error_value();
    }

    profile = maybe_profile.value();
  }

  ktl::optional<cpu_mask_t> cpu_mask;
  if ((info.flags & kAffinityFlags) != 0) {
    zx::result<cpu_mask_t> maybe_mask = parse_cpu_mask(info.cpu_affinity_mask);

    if (maybe_mask.is_error()) {
      return maybe_mask.error_value();
    }

    cpu_mask = maybe_mask.value();
  }

  ktl::optional<VmAddressRegion::MemoryPriority> memory_priority;
  if ((info.flags & kMemoryFlags) != 0) {
    zx::result<VmAddressRegion::MemoryPriority> maybe_memory_priority = parse_memory_priority(info);

    if (maybe_memory_priority.is_error()) {
      return maybe_memory_priority.error_value();
    }
    memory_priority = maybe_memory_priority.value();
  }

  fbl::AllocChecker ac;
  KernelHandle new_handle(
      fbl::AdoptRef(new (&ac) ProfileDispatcher(profile, cpu_mask, memory_priority)));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *rights = default_rights();
  *handle = ktl::move(new_handle);
  return ZX_OK;
}

ProfileDispatcher::ProfileDispatcher(
    const ktl::optional<SchedulerState::BaseProfile>& profile,
    const ktl::optional<cpu_mask_t>& cpu_mask,
    const ktl::optional<VmAddressRegion::MemoryPriority>& memory_priority)
    : profile_(profile), cpu_mask_(cpu_mask), memory_priority_(memory_priority) {
  kcounter_add(dispatcher_profile_create_count, 1);
}

ProfileDispatcher::~ProfileDispatcher() { kcounter_add(dispatcher_profile_destroy_count, 1); }

zx_status_t ProfileDispatcher::ApplyProfile(fbl::RefPtr<ThreadDispatcher> thread) {
  // Set the profile, if the user explicitly specified one when creating this profile.
  if (profile_.has_value()) {
    if (zx_status_t result = thread->SetBaseProfile(profile_.value()); result != ZX_OK) {
      return result;
    }
  }

  // Set cpu affinity, if the user explicitly specified one when creating this profile.
  if (cpu_mask_.has_value()) {
    return thread->SetSoftAffinity(cpu_mask_.value());
  }

  return ZX_OK;
}

zx_status_t ProfileDispatcher::ApplyProfile(fbl::RefPtr<VmAddressRegionDispatcher> vmar) {
  if (memory_priority_.has_value()) {
    return vmar->SetMemoryPriority(memory_priority_.value());
  }
  return ZX_OK;
}
