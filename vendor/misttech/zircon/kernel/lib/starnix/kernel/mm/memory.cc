// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/mm/memory.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/logging/logging.h>
#include <lib/mistos/util/status.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/features.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <ktl/span.h>
#include <object/handle.h>
#include <object/process_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_address_region.h>
#include <vm/vm_object.h>

#include "../kernel_priv.h"

namespace ktl {

using std::reference_wrapper;

}  // namespace ktl

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fbl::RefPtr<MemoryObject> MemoryObject::From(zx::vmo vmo) {
  fbl::AllocChecker ac;
  fbl::RefPtr<MemoryObject> memory = fbl::AdoptRef(new (&ac) MemoryObject(Vmo{ktl::move(vmo)}));
  ASSERT(ac.check());
  return ktl::move(memory);
}

ktl::optional<ktl::reference_wrapper<const zx::vmo>> MemoryObject::as_vmo() const {
  return ktl::visit(MemoryObject::overloaded{
                        [](const Vmo& vmo) {
                          return ktl::optional<ktl::reference_wrapper<const zx::vmo>>(vmo.vmo_);
                        },
                        [](const RingBuf&) {
                          return ktl::optional<ktl::reference_wrapper<const zx::vmo>>(ktl::nullopt);
                        },
                    },
                    variant_);
}

ktl::optional<zx::vmo> MemoryObject::into_vmo() {
  return ktl::visit(MemoryObject::overloaded{
                        [](Vmo& vmo) { return ktl::optional<zx::vmo>{ktl::move(vmo.vmo_)}; },
                        [](RingBuf&) { return ktl::optional<zx::vmo>(ktl::nullopt); },
                    },
                    variant_);
}

uint64_t MemoryObject::get_content_size() const {
  return ktl::visit(MemoryObject::overloaded{
                        [](const Vmo& vmo) {
                          uint64_t content_size = 0;
                          if (zx_status_t s = vmo.vmo_.get_prop_content_size(&content_size);
                              s != ZX_OK) {
                            impossible_error(s);
                          }
                          return content_size;
                        },
                        [](const RingBuf&) { return 0ul; },
                    },
                    variant_);
}

void MemoryObject::set_content_size(uint64_t size) const {
  ktl::visit(MemoryObject::overloaded{
                 [&size](const Vmo& vmo) {
                   if (zx_status_t s = vmo.vmo_.set_prop_content_size(size); s != ZX_OK) {
                     impossible_error(s);
                   }
                 },
                 [](const RingBuf&) {},
             },
             variant_);
}

uint64_t MemoryObject::get_size() const {
  return ktl::visit(MemoryObject::overloaded{
                        [](const Vmo& vmo) {
                          uint64_t size = 0;
                          if (zx_status_t s = vmo.vmo_.get_size(&size); s != ZX_OK) {
                            impossible_error(s);
                          }
                          return size;
                        },
                        [](const RingBuf&) { return 0ul; },
                    },
                    variant_);
}

fit::result<zx_status_t> MemoryObject::set_size(uint64_t size) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t> {
                          if (zx_status_t s = vmo.vmo_.set_size(size); s != ZX_OK) {
                            return fit::error(s);
                          }
                          return fit::ok();
                        },
                        [](const RingBuf&) -> fit::result<zx_status_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> MemoryObject::create_child(uint32_t options,
                                                                               uint64_t offset,
                                                                               uint64_t size) {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
                          zx::vmo child;
                          if (zx_status_t s = vmo.vmo_.create_child(options, offset, size, &child);
                              s != ZX_OK) {
                            return fit::error(s);
                          }
                          return fit::ok(MemoryObject::From(ktl::move(child)));
                        },
                        [](const RingBuf&) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> MemoryObject::duplicate_handle(
    zx_rights_t rights) const {
  return ktl::visit(
      MemoryObject::overloaded{
          [&rights](const Vmo& vmo) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
            zx::vmo dup;
            if (zx_status_t s = vmo.vmo_.duplicate(rights, &dup); s != ZX_OK) {
              return fit::error(s);
            }
            ZX_ASSERT(dup.is_valid());
            return fit::ok(MemoryObject::From(ktl::move(dup)));
          },
          [](const RingBuf& buf) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
            return fit::error(ZX_ERR_NOT_SUPPORTED);
          },
      },
      variant_);
}

fit::result<zx_status_t> MemoryObject::read(ktl::span<uint8_t>& data, uint64_t offset) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t> {
                          if (zx_status_t s = vmo.vmo_.read(data.data(), offset, data.size());
                              s != ZX_OK) {
                            return fit::error(s);
                          }
                          return fit::ok();
                        },
                        [](const RingBuf& buf) -> fit::result<zx_status_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

fit::result<zx_status_t> MemoryObject::read_uninit(ktl::span<uint8_t>& data,
                                                   uint64_t offset) const {
  return MemoryObject::read(data, offset);
}

fit::result<zx_status_t, fbl::Vector<uint8_t>> MemoryObject::read_to_vec(uint64_t offset,
                                                                         uint64_t length) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t, fbl::Vector<uint8_t>> {
                          fbl::AllocChecker ac;
                          fbl::Vector<uint8_t> buffer;
                          buffer.reserve(length, &ac);
                          if (!ac.check()) {
                            return fit::error(ZX_ERR_NO_MEMORY);
                          }
                          ktl::span<uint8_t> data{buffer.data(), length};
                          _EP(read_uninit(data, offset));
                          {
                            // SAFETY: since read_uninit succeeded we know that we can consider the
                            // buffer initialized.
                            buffer.set_size(length);
                          }
                          return fit::ok(ktl::move(buffer));
                        },
                        [](const RingBuf& buf) -> fit::result<zx_status_t, fbl::Vector<uint8_t>> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

fit::result<zx_status_t> MemoryObject::write(const ktl::span<const uint8_t>& data,
                                             uint64_t offset) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&data, &offset](const Vmo& vmo) -> fit::result<zx_status_t> {
                          if (zx_status_t s = vmo.vmo_.write(data.data(), offset, data.size());
                              s != ZX_OK) {
                            return fit::error(s);
                          }
                          return fit::ok();
                        },
                        [](const RingBuf& buf) -> fit::result<zx_status_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

zx_info_handle_basic_t MemoryObject::basic_info() const {
  zx_info_handle_basic_t info = {};
  zx_status_t status;
  ktl::visit(
      MemoryObject::overloaded{
          [&info, &status](const Vmo& vmo) {
            status = vmo.vmo_.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
          },
          [&info, &status](const RingBuf& buf) {
            status = buf.vmo_.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
          },
      },
      variant_);

  if (status != ZX_OK) {
    impossible_error(status);
  }
  return info;
}

fit::result<starnix_uapi::Errno, zx_info_vmo_t> MemoryObject::info() const {
  zx_info_vmo_t info = {};
  zx_status_t status;
  ktl::visit(MemoryObject::overloaded{
                 [&info, &status](const Vmo& vmo) {
                   status = vmo.vmo_.get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr);
                 },
                 [&info, &status](const RingBuf& buf) {
                   status = buf.vmo_.get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr);
                 },
             },
             variant_);

  if (status != ZX_OK) {
    return fit::error(errno(EIO));
  }
  return fit::ok(info);
}

void MemoryObject::set_zx_name(const char* name) const {
  ktl::span<const uint8_t> span{reinterpret_cast<const uint8_t*>(name), strlen(name)};
  ktl::visit(MemoryObject::overloaded{
                 [&span](const Vmo& vmo) { starnix::set_zx_name(vmo.vmo_, span); },
                 [&span](const RingBuf& buffer) { starnix::set_zx_name(buffer.vmo_, span); },
             },
             variant_);
}

fit::result<zx_status_t> MemoryObject::op_range(uint32_t op, uint64_t* offset,
                                                uint64_t* size) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t> {
                          if (zx_status_t s = vmo.vmo_.op_range(op, *offset, *size, nullptr, 0);
                              s != ZX_OK) {
                            return fit::error{s};
                          }
                          return fit::ok();
                        },
                        [](const RingBuf&) -> fit::result<zx_status_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

// zx_vmo_replace_as_executable
fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> MemoryObject::replace_as_executable() {
  return ktl::visit(MemoryObject::overloaded{
                        [&](Vmo& vmo) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
                          zx::vmo rep;
                          if (zx_status_t s = vmo.vmo_.replace_as_executable(zx::resource(), &rep);
                              s != ZX_OK) {
                            return fit::error{s};
                          }
                          ZX_ASSERT(rep.is_valid());
                          return fit::ok(MemoryObject::From(ktl::move(rep)));
                        },
                        [](RingBuf&) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

// zx_vmar_map
fit::result<zx_status_t, size_t> MemoryObject::map_in_vmar(const zx::vmar& vmar, size_t vmar_offset,
                                                           uint64_t* memory_offset, size_t len,
                                                           zx_vm_option_t flags) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t, size_t> {
                          zx_vaddr_t mapped_addr;
                          ZX_ASSERT(vmar.is_valid());
                          ZX_ASSERT(vmo.vmo_.is_valid());
                          if (zx_status_t s = vmar.map(flags, vmar_offset, vmo.vmo_, *memory_offset,
                                                       len, &mapped_addr);
                              s != ZX_OK) {
                            return fit::error(s);
                          }
                          return fit::ok(mapped_addr);
                        },
                        [](const RingBuf&) -> fit::result<zx_status_t, size_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

}  // namespace starnix
