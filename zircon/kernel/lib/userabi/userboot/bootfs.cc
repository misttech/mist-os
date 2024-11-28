// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "bootfs.h"

#include <lib/zbitl/error-stdio.h>
#include <lib/zircon-internal/align.h>
#include <lib/zx/vmo.h>
#include <stdarg.h>
#include <zircon/rights.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/log.h>
#include <zircon/types.h>

#include "util.h"

zx::vmo Bootfs::Open(std::string_view root, std::string_view filename, std::string_view purpose) {
  printl(log_, "searching BOOTFS for '%.*s%s%.*s' (%.*s)",    //
         static_cast<int>(root.size()), root.data(),          //
         root.empty() ? "" : "/",                             //
         static_cast<int>(filename.size()), filename.data(),  //
         static_cast<int>(purpose.size()), purpose.data());

  BootfsView bootfs = bootfs_reader_.root();
  auto it = root.empty() ? bootfs.find(filename) : bootfs.find({root, filename});
  if (auto result = bootfs.take_error(); result.is_error()) {
    Fail(result.error_value());
  }
  if (it == bootfs.end()) {
    printl(log_, "failed to find '%.*s%s%.*s'",         //
           static_cast<int>(root.size()), root.data(),  //
           root.empty() ? "" : "/",                     //
           static_cast<int>(filename.size()), filename.data());
    return {};
  }

  zx::vmo file_vmo;
  zx_status_t status;

  // When booting multiple programs, for example, in test cases or cuckoo testing, we need to check
  // if we have already generated a VMO for a given blob. If so, we can just duplicate the handle
  // and hand it over.
  if (booting_multiple_programs_) {
    for (const auto& entry : entries()) {
      if (entry.offset == it->offset) {
        status = entry.contents.duplicate(ZX_RIGHT_SAME_RIGHTS, &file_vmo);
        check(log_, status, "zx_handle_duplicate failed");
        return file_vmo;
      }
    }
  }

  size_t page_aligned_size = ZX_PAGE_ALIGN(it->size);
  status = zx::vmo::create(page_aligned_size, 0, &file_vmo);
  check(log_, status, "zx_vmo_create failed");
  status = zx_vmo_transfer_data(file_vmo.get(), 0, 0, page_aligned_size,
                                bootfs_reader_.storage().vmo().get(), it->offset);
  check(log_, status, "zx_vmo_transfer_data failed");

  status = file_vmo.set_property(ZX_PROP_NAME, filename.data(), filename.size());
  check(log_, status, "failed to set ZX_PROP_NAME");

  uint64_t size = it->size;
  status = file_vmo.set_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
  check(log_, status, "failed to set ZX_PROP_VMO_CONTENT_SIZE");

  status = file_vmo.replace_as_executable(vmex_resource_, &file_vmo);
  check(log_, status, "zx_vmo_replace_as_executable failed");

  // Duplicate `file_vmo` handle to be provided.
  zx::vmo extra;
  status = file_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &extra);
  check(log_, status, "zx_handle_duplicate failed.");

  ZX_ASSERT_MSG(entry_count_ < entries_.size(),
                "entry_count_=%zu be smaller than entries.size()=%zu for insertion", entry_count_,
                entries_.size());
  entries_[entry_count_++] = {.offset = it->offset, .contents = std::move(file_vmo)};
  return extra;
}

void Bootfs::Fail(const BootfsView::Error& error) {
  zbitl::PrintBootfsError(error, [this](const char* fmt, ...) {
    va_list args;
    va_start(args, fmt);
    printl(log_, fmt, args);
    va_end(args);
  });
  zx_process_exit(-1);
}
