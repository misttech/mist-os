// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/crashlog.h>
#include <lib/zbitl/error-stdio.h>
#include <lib/zbitl/image.h>
#include <lib/zbitl/memory.h>
#include <lib/zx/result.h>
#include <mexec.h>
#include <stdio.h>
#include <zircon/assert.h>
#include <zircon/limits.h>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/ref_ptr.h>
#include <ktl/byte.h>
#include <ktl/span.h>
#include <ktl/type_traits.h>
#include <ktl/variant.h>
#include <lk/init.h>
#include <phys/handoff.h>
#include <vm/vm_object.h>

#include <ktl/enforce.h>

namespace {

// Mexec data as gleaned from the physboot hand-off.
zbitl::View<ktl::span<const ktl::byte>> gImageAtHandoff;

void SetMexecDataZbi(uint level) { gImageAtHandoff = zbitl::View(gPhysHandoff->mexec_data.get()); }

}  // namespace

LK_INIT_HOOK(set_mexec_data_zbi, SetMexecDataZbi, LK_INIT_LEVEL_EARLIEST)

zx::result<size_t> WriteMexecData(ktl::span<ktl::byte> buffer) {
  // Storage or write errors resulting from a span-backed Image imply buffer
  // overflow.
  constexpr auto error = [](const auto& err) -> zx::result<size_t> {
    return zx::error{err.storage_error ? ZX_ERR_BUFFER_TOO_SMALL : ZX_ERR_INTERNAL};
  };
  constexpr auto extend_error = [](const auto& err) -> zx::result<size_t> {
    return zx::error{err.write_error ? ZX_ERR_BUFFER_TOO_SMALL : ZX_ERR_INTERNAL};
  };

  zbitl::Image image(buffer);
  if (auto result = image.clear(); result.is_error()) {
    zbitl::PrintViewError(result.error_value());
    return error(result.error_value());
  }

  if (auto result = image.Extend(gImageAtHandoff.begin(), gImageAtHandoff.end());
      result.is_error()) {
    zbitl::PrintViewCopyError(result.error_value());
    return extend_error(result.error_value());
  }

  if (auto result = gImageAtHandoff.take_error(); result.is_error()) {
    zbitl::PrintViewError(result.error_value());
    return zx::error{ZX_ERR_INTERNAL};
  }

  // Propagate any stashed crashlog to the next kernel.
  if (const fbl::RefPtr<VmObject> crashlog = crashlog_get_stashed()) {
    const zbi_header_t header = {
        .type = ZBI_TYPE_CRASHLOG,
        .length = static_cast<uint32_t>(crashlog->size()),
    };
    if (auto result = image.Append(header); result.is_error()) {
      printf("mexec: could not append crashlog: ");
      zbitl::PrintViewError(result.error_value());
      return error(result.error_value());
    } else {
      auto it = ktl::move(result).value();
      ktl::span<ktl::byte> payload = it->payload;
      zx_status_t status = crashlog->Read(payload.data(), 0, payload.size());
      if (status != ZX_OK) {
        return zx::error{status};
      }
    }
  }

  return zx::ok(image.size_bytes());
}
