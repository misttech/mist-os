// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/modules/bootfs/bootfs.h"

#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/util/status.h>
#include <lib/zbitl/error-stdio.h>
#include <lib/zbitl/view.h>
#include <zircon/assert.h>

#include <fbl/ref_ptr.h>
#include <ktl/move.h>
#include <vm/pinned_vm_object.h>
#include <vm/vm_object.h>

namespace bootfs {

using namespace starnix_uapi;

namespace {

using ZbiView = zbitl::View<fbl::RefPtr<VmObject>>;
using ZbiCopyError = ZbiView::CopyError<fbl::RefPtr<VmObject>>;

constexpr const char kBootfsVmoName[] = "uncompressed-bootfs";
constexpr const char kScratchVmoName[] = "bootfs-decompression-scratch";

// This is used as the zbitl::View::CopyStorageItem callback to allocate
// scratch memory used by decompression.
class ScratchAllocator {
 public:
  class Holder {
   public:
    Holder() = delete;
    Holder(const Holder&) = delete;
    Holder& operator=(const Holder&) = delete;

    // Unlike the default move constructor and move assignment operators, these
    // ensure that exactly one destructor cleans up the mapping.

    Holder(Holder&& other) { *this = ktl::move(other); }

    Holder& operator=(Holder&& other) {
      ktl::swap(mapping_, other.mapping_);
      ktl::swap(pinned_vmo_, other.pinned_vmo_);
      return *this;
    }

    Holder(size_t size) {
      fbl::RefPtr<VmObjectPaged> vmo;
      uint64_t aligned_size;
      zx_status_t status = VmObject::RoundSize(size, &aligned_size);
      ZX_ASSERT(status == ZX_OK);
      status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, aligned_size, &vmo);
      ZX_ASSERT(status == ZX_OK);
      status = vmo->set_name(kScratchVmoName, strlen(kScratchVmoName) - 1);
      ZX_ASSERT(status == ZX_OK);

      size = ROUNDUP_PAGE_SIZE(size);
      status = PinnedVmObject::Create(vmo, 0, size, /*write=*/true, &pinned_vmo_);
      ZX_ASSERT_MSG(status == ZX_OK, "Failed to make pin: %d\n", status);

      zx::result<VmAddressRegion::MapResult> map_result =
          VmAspace::kernel_aspace()->RootVmar()->CreateVmMapping(
              0, size, 0, VMAR_FLAG_CAN_MAP_READ | VMAR_FLAG_CAN_MAP_WRITE, pinned_vmo_.vmo(), 0,
              ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE, kScratchVmoName);

      if (map_result.is_error()) {
        ZX_PANIC("Failed to map in aspace\n");
      }

      if (status = map_result->mapping->MapRange(0, size, true); status != ZX_OK) {
        ZX_PANIC("Failed to map range\n");
      }

      mapping_ = ktl::move(map_result->mapping);
    }

    // zbitl::View::CopyStorageItem calls this get the scratch memory.
    void* get() const { return reinterpret_cast<void*>(mapping_->base_locking()); }

    ~Holder() {
      if (mapping_) {
        mapping_->Destroy();
        // pinned_memory_'s destructor will un-pin the pages just unmapped.
      }
    }

   private:
    PinnedVmObject pinned_vmo_;
    fbl::RefPtr<VmMapping> mapping_;
  };

  // zbitl::View::CopyStorageItem calls this to allocate scratch space.
  fit::result<ktl::string_view, Holder> operator()(size_t size) const {
    return fit::ok(Holder{size});
  }

  ScratchAllocator() = default;
};

[[noreturn]] void FailFromZbiCopyError(const ZbiCopyError& error) {
  zbitl::PrintViewCopyError(error, [&](const char* fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vprintf(fmt, args);
    va_end(args);
  });
  // zx_process_exit(-1);
  ZX_PANIC("");
}

/*
[[noreturn]] void Fail(const BootfsView::Error& error) {
  zbitl::PrintBootfsError(error, [&](const char* fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vprintf(fmt, args);
    va_end(args);
  });
  ZX_PANIC("");
}
*/


}  // namespace

FileSystemHandle BootFs::new_fs(const fbl::RefPtr<Kernel>& kernel, HandleOwner zbi_vmo) {
  if (auto result = BootFs::new_fs_with_options(kernel, ktl::move(zbi_vmo), {});
      result.is_error()) {
    ZX_PANIC("empty options cannot fail");
  } else {
    return result.value();
  }
}

fit::result<Errno, FileSystemHandle> BootFs::new_fs_with_options(const fbl::RefPtr<Kernel>& kernel,
                                                                 HandleOwner zbi_vmo,
                                                                 FileSystemOptions options) {
  fbl::AllocChecker ac;
  auto bootfs = new (&ac) BootFs(ktl::move(zbi_vmo));
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }

  auto fs = FileSystem::New(kernel, {CacheModeType::Permanent}, ktl::move(bootfs), options);
  auto mount_options = fs->options().params;

  /*auto result = [&]() -> fit::result<Errno, FileMode> {
    auto mode_str = mount_options.remove("mode");
    if (mode_str) {
      return FileMode::from_string({mode_str->data(), mode_str->size()});
    } else {
      return fit::ok(FILE_MODE(IFDIR, 0777));
    }
  }();

  if (result.is_error()) {
    return result.take_error();
  }
  FileMode mode = result.value();

  auto result_uid = [&]() -> fit::result<Errno, uid_t> {
    auto uid_str = mount_options.remove("uid");
    if (uid_str) {
      return parse<uid_t>({uid_str->data(), uid_str->size()});
    } else {
      return fit::ok(0);
    }
  }();
  if (result_uid.is_error()) {
    return result.take_error();
  }
  uid_t uid = result_uid.value();

  auto result_gid = [&]() -> fit::result<Errno, gid_t> {
    auto gid_str = mount_options.remove("gid");
    if (gid_str) {
      return parse<uid_t>({gid_str->data(), gid_str->size()});
    } else {
      return fit::ok(0);
    }
  }();
  if (result_gid.is_error()) {
    return result.take_error();
  }
  uid_t gid = result_gid.value();

  // BootfsDirectory::New()
  auto root_node =
      FsNode::new_root_with_properties(nullptr, [&mode, &uid, &gid](FsNodeInfo& info) -> void {
        info.chmod(mode);
        info.uid = uid;
        info.gid = gid;
      });
  fs->set_root_node(root_node);*/

  if (!mount_options.is_empty()) {
    /*track_stub!(
        TODO("https://fxbug.dev/322873419"),
        "unknown tmpfs options, see logs for strings"
    );*/
    /*log_warn!(
        "Unknown tmpfs options: {}",
        itertools::join(mount_options.iter().map(|(k, v)| format!("{k}={v}")), ",")
    );*/
  }

  return fit::ok(ktl::move(fs));
}

uint32_t from_be_bytes(const ktl::array<uint8_t, 4>& bytes) {
  uint32_t value;
  std::memcpy(&value, bytes.data(), sizeof(value));
  return ntohl(value);  // Convert from network byte order (big-endian)
}

fit::result<Errno, struct statfs> BootFs::statfs(const FileSystem& fs,
                                                 const CurrentTask& current_task) {
  struct statfs stat = default_statfs(from_be_bytes(ktl::array<uint8_t, 4>{'m', 'b', 'f', 's'}));
  // Pretend we have a ton of free space.
  stat.f_blocks = 0x100000000;
  stat.f_bavail = 0x100000000;
  stat.f_bfree = 0x100000000;
  return fit::ok(stat);
}

BootFs::BootFs(HandleOwner zbi_vmo) {
  fbl::RefPtr<Dispatcher> disp = zbi_vmo->dispatcher();
  ZbiView zbi(DownCastDispatcher<VmObjectDispatcher>(&disp)->vmo());
  fbl::RefPtr<VmObject> bootfs_vmo;
  for (auto it = zbi.begin(); it != zbi.end(); ++it) {
    if (it->header->type == ZBI_TYPE_STORAGE_BOOTFS) {
      auto result = zbi.CopyStorageItem(it, ScratchAllocator());
      if (result.is_error()) {
        printf("cannot extract BOOTFS from ZBI: ");
        FailFromZbiCopyError(result.error_value());
      }

      bootfs_vmo = ktl::move(result).value();
      zx_status_t status = bootfs_vmo->set_name(kBootfsVmoName, strlen(kBootfsVmoName) - 1);
      ZX_ASSERT(status == ZX_OK);

      // Signal that we've already processed this one.
      // GCC's -Wmissing-field-initializers is buggy: it should allow
      // designated initializers without all fields, but doesn't (in C++?).
      zbi_header_t discard{};
      discard.type = ZBI_TYPE_DISCARD;
      if (auto ok = zbi.EditHeader(it, discard); ok.is_error()) {
        ZX_PANIC("vmo write failed on ZBI VMO\n");
      }

      // Cancel error-checking since we're ending the iteration on purpose.
      zbi.ignore_error();
      break;
    }
  }

  if (bootfs_vmo) {
    if (auto result = BootfsReader::Create(std::move(bootfs_vmo)); result.is_error()) {
      // Fail(result.error_value());
    } else {
      bootfs_reader_ = std::move(result.value());
    }
  }
}

BootFs::~BootFs() = default;

}  // namespace bootfs
