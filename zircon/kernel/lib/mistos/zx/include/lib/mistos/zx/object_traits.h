// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_TRAITS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_TRAITS_H_

#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <vm/content_size_manager.h>
#include <vm/vm_address_region.h>
#include <vm/vm_object.h>

class ProcessDispatcher;
class ThreadDispatcher;
class JobDispatcher;
class EventDispatcher;

namespace zx {

class job;
class log;
class process;
class resource;
// class socket;
class event;
class thread;
class vmar;
class vmo;

class VmoStorage;

typedef void* raw_ptr_t;

// The default traits supports:
template <typename T>
struct object_traits {
  using StorageType = raw_ptr_t;
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = false;
  static constexpr bool supports_set_profile = false;
  static constexpr bool supports_user_signal = true;
  static constexpr bool supports_wait = false;
  static constexpr bool supports_kill = false;
  static constexpr bool has_peer_handle = false;
};

template <>
struct object_traits<event> {
  using StorageType = fbl::RefPtr<EventDispatcher>;
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = false;
  static constexpr bool supports_set_profile = false;
  static constexpr bool supports_user_signal = true;
  static constexpr bool supports_wait = false;
  static constexpr bool supports_kill = false;
  static constexpr bool has_peer_handle = false;
};

template <>
struct object_traits<log> {
  using StorageType = raw_ptr_t;
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = false;
  static constexpr bool supports_set_profile = false;
  static constexpr bool supports_user_signal = true;
  static constexpr bool supports_wait = false;
  static constexpr bool supports_kill = false;
  static constexpr bool has_peer_handle = false;
};

template <>
struct object_traits<vmo> {
  using StorageType = fbl::RefPtr<VmoStorage>;
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = false;
  static constexpr bool supports_set_profile = false;
  static constexpr bool supports_user_signal = true;
  static constexpr bool supports_wait = false;
  static constexpr bool supports_kill = false;
  static constexpr bool has_peer_handle = false;
};

template <>
struct object_traits<vmar> {
  using StorageType = fbl::RefPtr<VmAddressRegion>;
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = false;
  static constexpr bool supports_set_profile = true;
  static constexpr bool supports_user_signal = false;
  static constexpr bool supports_wait = false;
  static constexpr bool supports_kill = false;
  static constexpr bool has_peer_handle = false;
};

template <>
struct object_traits<job> {
  using StorageType = fbl::RefPtr<JobDispatcher>;
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = true;
  static constexpr bool supports_set_profile = false;
  static constexpr bool supports_user_signal = true;
  static constexpr bool supports_wait = true;
  static constexpr bool supports_kill = true;
  static constexpr bool has_peer_handle = false;
};

template <>
struct object_traits<process> {
  using StorageType = fbl::RefPtr<ProcessDispatcher>;
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = true;
  static constexpr bool supports_set_profile = false;
  static constexpr bool supports_user_signal = true;
  static constexpr bool supports_wait = true;
  static constexpr bool supports_kill = true;
  static constexpr bool has_peer_handle = false;
};

template <>
struct object_traits<thread> {
  using StorageType = fbl::RefPtr<ThreadDispatcher>;
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = false;
  static constexpr bool supports_set_profile = true;
  static constexpr bool supports_user_signal = true;
  static constexpr bool supports_wait = true;
  static constexpr bool supports_kill = false;
  static constexpr bool has_peer_handle = false;
};

template <>
struct object_traits<resource> {
  using StorageType = raw_ptr_t;
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = true;
  static constexpr bool supports_set_profile = false;
  static constexpr bool supports_user_signal = true;
  static constexpr bool supports_wait = true;
  static constexpr bool supports_kill = false;
  static constexpr bool has_peer_handle = false;
};

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_TRAITS_H_
