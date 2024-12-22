// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_BOOTFS_INCLUDE_LIB_STARNIX_BOOTFS_TESTS_ZBI_FILE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_BOOTFS_INCLUDE_LIB_STARNIX_BOOTFS_TESTS_ZBI_FILE_H_

#include <fbl/ref_ptr.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_object_paged.h>

namespace bootfs::testing {

// Copy of SymbolizerFile (zircon//kernel/lib/instrumentation/vmo.cc)
class ZbiFile {
 public:
  ZbiFile(ktl::string_view name = "ZbiFile") : name_(name) {
    zx_status_t status =
        VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kResizable, PAGE_SIZE, &vmo_);
    ZX_ASSERT(status == ZX_OK);
  }

  FILE* stream() { return &stream_; }

  int Write(ktl::string_view str) {
    zx_status_t status = vmo_->Write(str.data(), pos_, str.size());
    ZX_ASSERT(status == ZX_OK);
    pos_ += str.size();
    return static_cast<int>(str.size());
  }

  // Move the VMO into a handle and return it.
  HandleOwner Finish() && {
    KernelHandle<VmObjectDispatcher> handle;
    zx_rights_t rights;
    zx_status_t status = VmObjectDispatcher::Create(
        ktl::move(vmo_), pos_, VmObjectDispatcher::InitialMutability::kMutable, &handle, &rights);
    ZX_ASSERT(status == ZX_OK);
    status = handle.dispatcher()->set_name(name_.data(), name_.size());
    DEBUG_ASSERT(status == ZX_OK);
    return Handle::Make(ktl::move(handle), rights);
  }

 private:
  ktl::string_view name_;

  fbl::RefPtr<VmObjectPaged> vmo_;
  FILE stream_{this};
  size_t pos_ = 0;
};

}  // namespace bootfs::testing

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_BOOTFS_INCLUDE_LIB_STARNIX_BOOTFS_TESTS_ZBI_FILE_H_
