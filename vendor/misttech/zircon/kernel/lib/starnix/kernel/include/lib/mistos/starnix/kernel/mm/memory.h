// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/array.h>
#include <ktl/move.h>
#include <ktl/optional.h>
#include <ktl/span.h>
#include <ktl/variant.h>
#include <object/handle.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>

namespace starnix {

struct Vmar {
  HandleOwner vmar;

  fbl::RefPtr<VmAddressRegionDispatcher> dispatcher() const {
    fbl::RefPtr<Dispatcher> disp = vmar->dispatcher();
    return DownCastDispatcher<VmAddressRegionDispatcher>(&disp);
  }
};

struct Vmo {
  HandleOwner vmo;

  fbl::RefPtr<VmObjectDispatcher> dispatcher() const {
    fbl::RefPtr<Dispatcher> disp = vmo->dispatcher();
    return DownCastDispatcher<VmObjectDispatcher>(&disp);
  }
};

/// The memory object is a bpf ring buffer. The layout it represents is:
/// |Page1 - Page2 - Page3 .. PageN - Page3 .. PageN| where the vmo is
/// |Page1 - Page2 - Page3 .. PageN|
struct RingBuf {
  HandleOwner vmo;

  fbl::RefPtr<VmObjectDispatcher> dispatcher() const {
    fbl::RefPtr<Dispatcher> disp = vmo->dispatcher();
    return DownCastDispatcher<VmObjectDispatcher>(&disp);
  }
};

class MemoryObject : public fbl::RefCounted<MemoryObject> {
 public:
  // impl MemoryObject

  static fbl::RefPtr<MemoryObject> From(HandleOwner vmo);

  ktl::optional<std::reference_wrapper<Vmo>> as_vmo();

  ktl::optional<Vmo> into_vmo();

  uint64_t get_content_size() const;

  void set_content_size(uint64_t size) const;

  uint64_t get_size() const;

  fit::result<zx_status_t> set_size(uint64_t size) const;

  fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> create_child(uint32_t options,
                                                                   uint64_t offset, uint64_t size);

  fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> duplicate_handle(zx_rights_t rights) const;

  fit::result<zx_status_t> read(ktl::span<uint8_t>& data, uint64_t offset) const;

  template <typename T, std::size_t N>
  fit::result<zx_status_t, ktl::array<T, N>> read_to_array(uint64_t offset) const {
    return ktl::visit(MemoryObject::overloaded{
                          [&](const Vmo& vmo) -> fit::result<zx_status_t, ktl::array<T, N>> {
                            ktl::array<T, N> array;
                            ktl::span<uint8_t> buf{(uint8_t*)array.data(), array.size()};
                            auto result = read_uninit(buf, offset);
                            if (result.is_error()) {
                              return result.take_error();
                            }
                            return fit::ok(ktl::move(array));
                          },
                          [](const RingBuf& buf) -> fit::result<zx_status_t, ktl::array<T, N>> {
                            return fit::error(ZX_ERR_NOT_SUPPORTED);
                          },
                      },
                      variant_);
  }

  fit::result<zx_status_t, fbl::Vector<uint8_t>> read_to_vec(uint64_t offset,
                                                             uint64_t length) const;

  fit::result<zx_status_t> read_uninit(ktl::span<uint8_t>& data, uint64_t offset) const;

  fit::result<zx_status_t> write(const ktl::span<const uint8_t>& data, uint64_t offset) const;

  zx_info_handle_basic_t basic_info() const;

  zx_koid_t get_koid() const { return basic_info().koid; }

  zx_info_vmo_t info() const;

  void set_zx_name(const char* name) const;

  fit::result<zx_status_t> op_range(uint32_t op, uint64_t* offset, uint64_t* size) const;

  fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> replace_as_executable();

  fit::result<zx_status_t, size_t> map_in_vmar(const Vmar& vmar, size_t vmar_offset,
                                               uint64_t* memory_offset, size_t len,
                                               zx_vm_option_t flags) const;

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

fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> create_vmo(uint64_t size, uint32_t options);
fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> create_child_vmo(const Vmo& vmo,
                                                                     uint32_t options,
                                                                     uint64_t offset,
                                                                     uint64_t size);

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_MM_MEMORY_H_
