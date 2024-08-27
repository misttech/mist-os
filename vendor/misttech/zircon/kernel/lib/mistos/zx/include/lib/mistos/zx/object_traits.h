// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_TRAITS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_OBJECT_TRAITS_H_

#include <zircon/types.h>

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

// The default traits supports:
// - bti
// - event
// - iommu
// - profile
// - timer
// - vmo
template <typename T>
struct object_traits {
  static constexpr bool supports_duplication = true;
  static constexpr bool supports_get_child = false;
  static constexpr bool supports_set_profile = false;
  static constexpr bool supports_user_signal = true;
  static constexpr bool supports_wait = true;
  static constexpr bool supports_kill = false;
  static constexpr bool has_peer_handle = false;
};

template <>
struct object_traits<event> {
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
