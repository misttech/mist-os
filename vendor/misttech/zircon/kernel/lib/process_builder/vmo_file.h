// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_VMO_FILE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_VMO_FILE_H_

#include <lib/elfldltl/file.h>
#include <lib/elfldltl/zircon.h>
#include <lib/fit/result.h>
#include <lib/stdcompat/span.h>
#include <zircon/errors.h>

#include <fbl/ref_ptr.h>
#include <vm/vm_object.h>

// VmoFile is constructible from fbl::RefPtr<VmObject> and meets the File API
// (see <lib/elfldltl/memory.h>) by calling read.

namespace process_builder {

namespace internal {

inline fit::result<elfldltl::ZirconError> ReadVmo(const fbl::RefPtr<VmObject>& vmo, uint64_t offset,
                                                  cpp20::span<std::byte> buffer) {
  zx_status_t status = vmo->Read(buffer.data(), offset, buffer.size());
  if (status == ZX_ERR_OUT_OF_RANGE) {
    return fit::error{elfldltl::ZirconError{}};  // This indicates EOF.
  }
  if (status != ZX_OK) {
    return fit::error{elfldltl::ZirconError{status}};
  }
  return fit::ok();
}

}  // namespace internal

template <class Diagnostics>
using VmoFileBase = elfldltl::File<Diagnostics, fbl::RefPtr<VmObject>, uint64_t, internal::ReadVmo>;

template <class Diagnostics>
class VmoFile : public VmoFileBase<Diagnostics> {
 public:
  using VmoFileBase<Diagnostics>::VmoFileBase;

  VmoFile(VmoFile&&) = default;

  VmoFile& operator=(VmoFile&&) = default;

  fbl::RefPtr<VmObject> borrow() const { return this->get(); }
};

// Deduction guide.
template <class Diagnostics>
VmoFile(fbl::RefPtr<VmObject>, Diagnostics&& diagnostics) -> VmoFile<Diagnostics>;

}  // namespace process_builder

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_VMO_FILE_H_
