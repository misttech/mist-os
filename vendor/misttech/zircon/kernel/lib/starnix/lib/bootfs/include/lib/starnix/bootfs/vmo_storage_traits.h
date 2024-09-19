// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_MODULES_BOOTFS_INCLUDE_LIB_MODULES_BOOTFS_VMO_STORAGE_TRAITS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_MODULES_BOOTFS_INCLUDE_LIB_MODULES_BOOTFS_VMO_STORAGE_TRAITS_H_

#include <lib/mistos/util/status.h>
#include <lib/zbitl/storage-traits.h>

#include <vm/vm_object.h>

namespace zbitl {

template <>
struct StorageTraits<fbl::RefPtr<VmObject>> {
 public:
  /// Errors from  calls.
  using error_type = zx_status_t;

  /// Offset into the VMO where the ZBI item payload begins.
  using payload_type = uint64_t;

  // Exposed for testing.
  static constexpr size_t kBufferedReadChunkSize = 8192;

  static std::string_view error_string(error_type error) { return zx_status_get_string(error); }

  // Returns ZX_PROP_VMO_CONTENT_SIZE, if set - or else the page-rounded VMO
  // size.
  static fit::result<error_type, uint32_t> Capacity(const fbl::RefPtr<VmObject>&);

  // Will enlarge the underlying VMO size if needed, updating
  // ZX_PROP_VMO_CONTENT_SIZE to the new capacity value if so.
  static fit::result<error_type> EnsureCapacity(const fbl::RefPtr<VmObject>&,
                                                uint32_t capacity_bytes);

  static fit::result<error_type, payload_type> Payload(const fbl::RefPtr<VmObject>&,
                                                       uint32_t offset, uint32_t length) {
    return fit::ok(offset);
  }

  static fit::result<error_type> Read(const fbl::RefPtr<VmObject>& zbi, payload_type payload,
                                      void* buffer, uint32_t length);

  template <typename Callback>
  static auto Read(const fbl::RefPtr<VmObject>& zbi, payload_type payload, uint32_t length,
                   Callback&& callback) -> fit::result<error_type, decltype(callback(ByteView{}))> {
    std::optional<decltype(callback(ByteView{}))> result;
    auto cb = [&](ByteView chunk) -> bool {
      result = callback(chunk);
      return result->is_ok();
    };
    using CbType = decltype(cb);
    if (auto read_error = DoRead(
            zbi, payload, length,
            [](void* cb, ByteView chunk) { return (*static_cast<CbType*>(cb))(chunk); }, &cb);
        read_error.is_error()) {
      return fit::error{read_error.error_value()};
    } else {
      ZX_DEBUG_ASSERT(result);
      return fit::ok(*result);
    }
  }

  static fit::result<error_type> Write(const fbl::RefPtr<VmObject>&, uint32_t offset, ByteView);

  static fit::result<error_type, fbl::RefPtr<VmObject>> Create(const fbl::RefPtr<VmObject>&,
                                                               uint32_t size,
                                                               uint32_t initial_zero_size);

  template <typename SlopCheck>
  static fit::result<error_type, std::optional<std::pair<fbl::RefPtr<VmObject>, uint32_t>>> Clone(
      const fbl::RefPtr<VmObject>& zbi, uint32_t offset, uint32_t length, uint32_t to_offset,
      SlopCheck&& slopcheck) {
    if (slopcheck(offset % ZX_PAGE_SIZE)) {
      return DoClone(zbi, offset, length);
    }
    return fit::ok(std::nullopt);
  }

 private:
  static fit::result<error_type> DoRead(const fbl::RefPtr<VmObject>& zbi, uint64_t offset,
                                        uint32_t length, bool (*)(void*, ByteView), void*);

  static fit::result<error_type, std::optional<std::pair<fbl::RefPtr<VmObject>, uint32_t>>> DoClone(
      const fbl::RefPtr<VmObject>& zbi, uint32_t offset, uint32_t length);
};

}  // namespace zbitl

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_MODULES_BOOTFS_INCLUDE_LIB_MODULES_BOOTFS_VMO_STORAGE_TRAITS_H_
