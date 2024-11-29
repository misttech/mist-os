// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_BOOTFS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_BOOTFS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/file_system_ops.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/bstring.h>
#include <lib/mistos/util/range-map.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <lib/starnix/bootfs/vmo.h>
#include <lib/starnix_sync/locks.h>
#include <lib/zbitl/items/bootfs.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <object/handle.h>

namespace starnix {

using BootfsReader = zbitl::Bootfs<zx::vmo>;
using BootfsView = zbitl::BootfsView<zx::vmo>;

class BootfsError : public mtl::StdError {
 public:
  enum class Code {
    kInvalidHandle,
    kDuplicateHandle,
    kVmex,
    kParser,
    kAddEntry,
    kMissingEntry,
    kExecVmo,
    kVmo,
    kVmoName,
    kConvertNumber,
    kConvertString,
    kNamespace,
    kMisalignedOffset,
  };

  explicit BootfsError(Code code) : code_(code) {}
  explicit BootfsError(Code code, ktl::string_view name) : code_(code), name_(name) {}
  explicit BootfsError(Code code, uint32_t offset) : code_(code), offset_(offset) {}
  explicit BootfsError(Code code, zx_status_t status) : code_(code), status_(status) {}

  mtl::BString to_string() const override {
    switch (code_) {
      case Code::kInvalidHandle:
        return "Invalid handle";
      case Code::kDuplicateHandle:
        return mtl::format("Failed to duplicate handle: %d", status_);
      case Code::kVmex:
        return mtl::format("Failed to access vmex Resource: %d", status_);
      case Code::kParser:
        return "BootfsParser error";
      case Code::kAddEntry:
        return "Failed to add entry to Bootfs VFS";
      case Code::kMissingEntry:
        return mtl::format("Failed to locate entry for %.*s", static_cast<int>(name_.size()),
                           name_.data());
      case Code::kExecVmo:
        return mtl::format("Failed to create an executable VMO: %d", status_);
      case Code::kVmo:
        return mtl::format("VMO operation failed: %d", status_);
      case Code::kVmoName:
        return mtl::format("Failed to get VMO name: %d", status_);
      case Code::kConvertNumber:
        return "Failed to convert numerical value";
      case Code::kConvertString:
        return "Failed to convert string value";
      case Code::kNamespace:
        return mtl::format("Failed to bind Bootfs to Component Manager's namespace: %d", status_);
      case Code::kMisalignedOffset:
        return mtl::format("Bootfs entry at offset %u is not page-aligned", offset_);
    }
  }

 private:
  Code code_;
  ktl::string_view name_;
  uint32_t offset_ = 0;
  zx_status_t status_ = ZX_OK;
};

class BootFs final : public FileSystemOps {
 public:
  static FileSystemHandle new_fs(const fbl::RefPtr<Kernel>& kernel, zx::unowned_vmo vmo);

  static fit::result<Errno, FileSystemHandle> new_fs_with_options(const fbl::RefPtr<Kernel>& kernel,
                                                                  zx::unowned_vmo vmo,
                                                                  FileSystemOptions options);

  fit::result<Errno, struct statfs> statfs(const FileSystem& fs,
                                           const CurrentTask& current_task) final;

  const FsStr& name() final { return name_; }

 private:
  // Create a VMO and transfer the entry's data from Bootfs to it. This
  // operation will also decommit the transferred range in the Bootfs image.
  fit::result<BootfsError, zx::vmo> create_vmo_from_bootfs(const util::Range<uint64_t>& range,
                                                           uint64_t original_size);

 public:
  // C++
  explicit BootFs(zx::unowned_vmar vmar, zx::unowned_vmo bootfs);

  ~BootFs() final;

 private:
  const FsStr name_ = "bootfs";

  BootfsReader bootfs_reader_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_BOOTFS_H_
