// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/devmgr/driver.h"

#include <lib/fit/function.h>
#include <trace.h>

#include <fbl/alloc_checker.h>
#include <ktl/unique_ptr.h>

#define VERBOSE_DRIVER_LOAD 1

#define LOCAL_TRACE 0

namespace devmgr {

namespace {

struct AddContext {
  void* addr;
  DriverLoadCallback func;
};

void found_driver(const zircon_driver_note_payload_t* note, const zx_driver_rec_t* rec,
                  const zx_bind_inst_t* bi, void* cookie) {
  auto context = static_cast<const AddContext*>(cookie);

  fbl::AllocChecker ac;
  auto drv = fbl::MakeRefCountedChecked<Driver>(&ac, note->name, context->addr, rec);
  ZX_ASSERT(ac.check());

  const size_t bindlen = note->bindcount * sizeof(zx_bind_inst_t);
  auto binding = fbl::make_unique_checked<zx_bind_inst_t[]>(ac, note->bindcount);
  ZX_ASSERT(ac.check());
  memcpy(binding.get(), bi, bindlen);

  drv->set_binding(std::move(binding));
  drv->set_binding_size(static_cast<uint32_t>(bindlen));

#if VERBOSE_DRIVER_LOAD
  dprintf(INFO, "found driver: %p\n", context->addr);
  dprintf(INFO, "        name: %s\n", note->name);
  dprintf(INFO, "      vendor: %s\n", note->vendor);
  dprintf(INFO, "     version: %s\n", note->version);
  dprintf(INFO, "       flags: %#x\n", note->flags);
  dprintf(INFO, "     binding:\n");
  for (size_t n = 0; n < note->bindcount; n++) {
    dprintf(INFO, "         %03zd: %08x %08x\n", n, bi[n].op, bi[n].arg);
  }
#endif

  context->func(std::move(drv), note->version);
}

}  // namespace

void load_driver(const char* name, DriverLoadCallback func) {}

typedef struct {
  zircon_driver_note_t note;
  zx_driver_rec_t rec;
  zx_bind_inst_t binding[0];
} mistos_driver_ldr_t;

void find_loadable_drivers(const void* start, const void* end, DriverLoadCallback func) {
  AddContext context = {.addr = nullptr, .func = std::move(func)};

  const mistos_driver_ldr_t* first = static_cast<const mistos_driver_ldr_t*>(start);
  const mistos_driver_ldr_t* last = static_cast<const mistos_driver_ldr_t*>(end);

  for (const mistos_driver_ldr_t* drv = first; drv != last;) {
    context.addr = const_cast<mistos_driver_ldr_t*>(drv);
    found_driver(&drv->note.payload, &drv->rec, drv->binding, &context);
    drv = reinterpret_cast<const mistos_driver_ldr_t*>(
        reinterpret_cast<const char*>(drv) + sizeof(mistos_driver_ldr_t) +
        (drv->note.payload.bindcount * sizeof(zx_bind_inst_t)));
  }
}

Driver::Driver(std::string_view url, void* library, DriverHooks hooks)
    : url_(url), library_(library), hooks_(hooks) {}

Driver::~Driver() {}

void Driver::set_binding(ktl::unique_ptr<const zx_bind_inst_t[]> binding) {
  //  fbl::AutoLock al(&lock_);
  binding_.emplace(std::move(binding));
}

void Driver::Start(fbl::RefPtr<Driver> self, /*fuchsia_driver_framework::DriverStartArgs start_args,
             fdf::Dispatcher dispatcher,*/
                   fit::callback<void(zx::result<>)> cb) {
  cb(zx::ok());
}

}  // namespace devmgr
