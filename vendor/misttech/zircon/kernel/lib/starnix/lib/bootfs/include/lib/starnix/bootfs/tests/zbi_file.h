// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_BOOTFS_INCLUDE_LIB_STARNIX_BOOTFS_TESTS_ZBI_FILE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_BOOTFS_INCLUDE_LIB_STARNIX_BOOTFS_TESTS_ZBI_FILE_H_

#include <object/vm_object_dispatcher.h>
#include <vm/vm_object_paged.h>

namespace bootfs::testing {

// This object facilitates doing fprintf directly into the VMO representing
// the symbolizer markup data file.  This gets the symbolizer context for the
// kernel and then a dumpfile element for each VMO published.
class ZbiFile {
 public:
  ZbiFile() {
    zx_status_t status =
        VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kResizable, PAGE_SIZE, &vmo_);
    ZX_ASSERT(status == ZX_OK);
  }

  int Write(ktl::string_view str) {
    zx_status_t status = vmo_->Write(str.data(), pos_, str.size());
    ZX_ASSERT(status == ZX_OK);
    pos_ += str.size();
    return static_cast<int>(str.size());
  }

  // Move the VMO into a handle and return it.
  Handle* Finish() && {
    KernelHandle<VmObjectDispatcher> handle;
    zx_rights_t rights;
    zx_status_t status = VmObjectDispatcher::Create(
        ktl::move(vmo_), 0, VmObjectDispatcher::InitialMutability::kMutable, &handle, &rights);
    ZX_ASSERT(status == ZX_OK);
    // status = handle.dispatcher()->set_name(kVmoName.data(), kVmoName.size());
    // DEBUG_ASSERT(status == ZX_OK);
    status = handle.dispatcher()->SetContentSize(pos_);
    DEBUG_ASSERT(status == ZX_OK);
    return Handle::Make(ktl::move(handle), rights).release();
  }

 private:
  fbl::RefPtr<VmObjectPaged> vmo_;
  size_t pos_ = 0;
};

}  // namespace bootfs::testing

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_BOOTFS_INCLUDE_LIB_STARNIX_BOOTFS_TESTS_ZBI_FILE_H_
