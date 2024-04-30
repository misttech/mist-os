// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/mistos/zbi_parser/zbi.h"

#include <lib/mistos/util/status.h>
#include <lib/mistos/zbi_parser/option.h>
#include <lib/mistos/zbitl/vmo.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/error-stdio.h>
#include <lib/zbitl/view.h>
#include <lib/zircon-internal/align.h>
#include <stdarg.h>
#include <trace.h>

#include <ktl/string_view.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

namespace {

using ZbiView = zbitl::View<zbitl::MapUnownedVmo>;
using ZbiError = ZbiView::Error;
using ZbiCopyError = ZbiView::CopyError<zbitl::MapOwnedVmo>;

constexpr const char kBootfsVmoName[] = "uncompressed-bootfs";
constexpr const char kScratchVmoName[] = "bootfs-decompression-scratch";

void FailFromZbiError(const ZbiError& error) {
  zbitl::PrintViewError(error, [&](const char* fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vprintf(fmt, args);
    va_end(args);
  });
}

void FailFromZbiCopyError(const ZbiCopyError& error) {
  zbitl::PrintViewCopyError(error, [&](const char* fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vprintf(fmt, args);
    va_end(args);
  });
}

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

    Holder(Holder&& other) {
      *this = ktl::move(other);
      ZX_ASSERT(*vmar_);
    }

    Holder& operator=(Holder&& other) {
      ktl::swap(vmar_, other.vmar_);
      ktl::swap(mapping_, other.mapping_);
      ktl::swap(size_, other.size_);
      ZX_ASSERT(*vmar_);
      return *this;
    }

    Holder(const zx::vmar& vmar, size_t size) : vmar_(vmar), size_(size) {
      ZX_ASSERT(*vmar_);
      zx::vmo vmo;
      Do(zx::vmo::create(size, 0, &vmo), "allocate");
      Do(vmar_->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, size, &mapping_), "map");
      Do(vmo.set_property(ZX_PROP_NAME, kScratchVmoName, sizeof(kScratchVmoName) - 1), "name");
    }

    // zbitl::View::CopyStorageItem calls this get the scratch memory.
    void* get() const { return reinterpret_cast<void*>(mapping_); }

    ~Holder() {
      if (mapping_ != 0) {
        Do(vmar_->unmap(mapping_, size_), "unmap");
      }
    }

   private:
    void Do(zx_status_t status, const char* what) {
      if (status != ZX_OK) {
        PANIC("cannot %s %zu-byte VMO for %s [%d]", what, size_, kScratchVmoName, status);
      }
      LTRACEF_LEVEL(2, "OK %s %zu-byte VMO for %s\n", what, size_, kScratchVmoName);
    }

    zx::unowned_vmar vmar_;
    uintptr_t mapping_ = 0;
    size_t size_ = 0;
  };

  ScratchAllocator() = delete;

  ScratchAllocator(const zx::vmar& vmar_self) : vmar_(zx::unowned_vmar{vmar_self}) {
    ZX_ASSERT(*vmar_);
  }

  // zbitl::View::CopyStorageItem calls this to allocate scratch space.
  fit::result<ktl::string_view, Holder> operator()(size_t size) const {
    return fit::ok(Holder{*vmar_, size});
  }

 private:
  zx::unowned_vmar vmar_;
};

}  // namespace

#define check(log, status, fmt, ...)                                   \
  do {                                                                 \
    if (status != ZX_OK) {                                             \
      TRACEF("%s: " fmt, zx_status_get_string(status), ##__VA_ARGS__); \
      return fit::error(status);                                       \
    }                                                                  \
  } while (0)

namespace zbi_parser {

fit::result<zx_status_t, zx::vmo> GetBootfsFromZbi(const zx::vmar& vmar_self,
                                                   const zx::vmo& zbi_vmo, bool discard) {
  ZbiView zbi(zbitl::MapUnownedVmo{zx::unowned_vmo{zbi_vmo}, /*writable=*/true,
                                   zx::unowned_vmar{vmar_self}});

  for (auto it = zbi.begin(); it != zbi.end(); ++it) {
    if (it->header->type == ZBI_TYPE_STORAGE_BOOTFS) {
      auto result = zbi.CopyStorageItem(it, ScratchAllocator{vmar_self});
      if (result.is_error()) {
        LTRACEF_LEVEL(2, "cannot extract BOOTFS from ZBI: ");
        FailFromZbiCopyError(result.error_value());
      }

      zx::vmo bootfs_vmo = ktl::move(result).value().release();
      check(log, bootfs_vmo.set_property(ZX_PROP_NAME, kBootfsVmoName, sizeof(kBootfsVmoName) - 1),
            "cannot set name of uncompressed BOOTFS VMO\n");

      if (discard) {
        // Signal that we've already processed this one.
        // GCC's -Wmissing-field-initializers is buggy: it should allow
        // designated initializers without all fields, but doesn't (in C++?).
        zbi_header_t discard_{};
        discard_.type = ZBI_TYPE_DISCARD;
        if (auto ok = zbi.EditHeader(it, discard_); ok.is_error()) {
          check(log, ok.error_value(), "zx_vmo_write failed on ZBI VMO\n");
        }
      }

      // Cancel error-checking since we're ending the iteration on purpose.
      zbi.ignore_error();
      return fit::ok(ktl::move(bootfs_vmo));
    }
  }

  if (auto check = zbi.take_error(); check.is_error()) {
    LTRACEF_LEVEL(2, "invalid ZBI: ");
    FailFromZbiError(check.error_value());
  }

  LTRACEF_LEVEL(2, "no '/boot' bootfs in bootstrap message\n");
  return fit::ok(zx::vmo());
}

fit::result<zx_status_t, Options> GetOptionsFromZbi(const zx::vmar& vmar_self, const zx::vmo& zbi) {
  ZbiView view(zbitl::MapUnownedVmo{
      zx::unowned_vmo{zbi},
      /*writable=*/false,
      zx::unowned_vmar{vmar_self},
  });
  Options opts;
  for (auto [header, payload] : view) {
    if (header->type != ZBI_TYPE_CMDLINE) {
      continue;
    }

    // Map in and parse the CMDLINE payload. The strings referenced by `opts`
    // will be owned by the mapped pages and will be valid within `vmar_self`'s
    // lifetime (i.e., for the entirety of userboot's runtime).
    const uint64_t previous_page_boundary = payload & -ZX_PAGE_SIZE;
    const uint64_t next_page_boundary = ZX_PAGE_ALIGN(payload + header->length);
    const size_t size = next_page_boundary - previous_page_boundary;
    uintptr_t mapping;
    if (zx_status_t status =
            vmar_self.map(ZX_VM_PERM_READ, 0, zbi, previous_page_boundary, size, &mapping);
        status != ZX_OK) {
      LTRACEF_LEVEL(2, "failed to map CMDLINE item: %s\n", zx_status_get_string(status));
    }

    const uintptr_t mapped_payload = mapping + (payload % ZX_PAGE_SIZE);
    ktl::string_view cmdline{reinterpret_cast<const char*>(mapped_payload), header->length};
    LTRACEF_LEVEL(2, "CMDLINE %.*s\n", static_cast<int>(cmdline.size()), cmdline.data());
    ParseCmdline(cmdline, opts);
  }

  if (auto result = view.take_error(); result.is_error()) {
    LTRACEF_LEVEL(2, "invalid ZBI: ");
    FailFromZbiError(result.error_value());
  }

  return fit::ok(opts);
}

}  // namespace zbi_parser
