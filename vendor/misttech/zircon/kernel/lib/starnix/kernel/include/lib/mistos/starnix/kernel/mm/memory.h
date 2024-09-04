// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <zircon/rights.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <ktl/move.h>
#include <ktl/optional.h>
#include <ktl/span.h>
#include <ktl/variant.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>

namespace starnix {

struct Vmar {
  fbl::RefPtr<VmAddressRegionDispatcher> vmar;
  zx_rights_t rights;

  bool HasRights(zx_rights_t desired) const { return (rights & desired) == desired; }
};

struct Vmo {
  fbl::RefPtr<VmObjectDispatcher> vmo;
  zx_rights_t rights;

  bool HasRights(zx_rights_t desired) const { return (rights & desired) == desired; }
};

/// The memory object is a bpf ring buffer. The layout it represents is:
/// |Page1 - Page2 - Page3 .. PageN - Page3 .. PageN| where the vmo is
/// |Page1 - Page2 - Page3 .. PageN|
struct RingBuf {
  fbl::RefPtr<VmObjectDispatcher> vmo;
};

class MemoryObject : public fbl::RefCounted<MemoryObject> {
 public:
  // impl MemoryObject

  static fbl::RefPtr<MemoryObject> From(fbl::RefPtr<VmObjectDispatcher> vmo, zx_rights_t rights);

  ktl::optional<fbl::RefPtr<VmObjectDispatcher>> as_vmo();

  ktl::optional<fbl::RefPtr<VmObjectDispatcher>> into_vmo();

  uint64_t get_content_size() const;

  void set_content_size(uint64_t size) const;

  uint64_t get_size() const;

  fit::result<zx_status_t> set_size(uint64_t size) const;

  fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> create_child(uint32_t options,
                                                                   uint64_t offset, uint64_t size);

  fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> duplicate_handle(zx_rights_t rights) const;

  fit::result<zx_status_t> read(ktl::span<uint8_t>& data, uint64_t offset) const;

  fit::result<zx_status_t> read_uninit(ktl::span<uint8_t>& data, uint64_t offset) const;

  fit::result<zx_status_t> write(const ktl::span<const uint8_t>& data, uint64_t offset) const;

  zx_info_handle_basic_t basic_info() const;

  zx_koid_t get_koid() const { return basic_info().koid; }

  zx_info_vmo_t info() const;

  void set_zx_name(const char* name) const;

  fit::result<zx_status_t> op_range(uint32_t op, uint64_t* offset, uint64_t* size) const;

  fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> replace_as_executable() const;

  fit::result<zx_status_t, size_t> map_in_vmar(Vmar vmar, size_t vmar_offset,
                                               uint64_t* memory_offset, size_t len,
                                               MappingFlags flags) const;

 private:
  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

 private:
  MemoryObject(Vmo vmo) : variant_(ktl::move(vmo)) {}
  MemoryObject(RingBuf buf) : variant_(ktl::move(buf)) {}

  ktl::variant<Vmo, RingBuf> variant_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_H_
