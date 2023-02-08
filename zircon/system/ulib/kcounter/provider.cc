// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/kcounter/provider.h>
#include <lib/zx/channel.h>
#include <string.h>
#include <zircon/status.h>

#include "kcounter.h"

namespace {

class Counter : public fidl::WireServer<fuchsia_kernel::Counter> {
 public:
  explicit Counter(kcounter::VmoToInspectMapper& mapper) : mapper_(mapper) {}

 private:
  void GetInspectVmo(GetInspectVmoCompleter::Sync& completer) override {
    fuchsia_mem::wire::Buffer buffer;
    if (zx_status_t status = mapper_.GetInspectVMO(&buffer.vmo); status != ZX_OK) {
      return completer.Reply(status, {});
    }
    if (zx_status_t status = buffer.vmo.get_size(&buffer.size); status != ZX_OK) {
      return completer.Reply(status, {});
    }
    completer.Reply(ZX_OK, std::move(buffer));
  }

  void UpdateInspectVmo(UpdateInspectVmoCompleter::Sync& completer) override {
    completer.Reply(mapper_.UpdateInspectVMO());
  }

  kcounter::VmoToInspectMapper& mapper_;
};

zx_status_t Connect(void* ctx, async_dispatcher_t* dispatcher, const char* service_name,
                    zx_handle_t request) {
  zx::channel channel{request};
  if (fidl::DiscoverableProtocolName<fuchsia_kernel::Counter> == service_name) {
    kcounter::VmoToInspectMapper& mapper = *static_cast<kcounter::VmoToInspectMapper*>(ctx);
    fidl::BindServer(dispatcher, fidl::ServerEnd<fuchsia_kernel::Counter>{std::move(channel)},
                     std::make_unique<Counter>(mapper));
    return ZX_OK;
  }

  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Init(void** out_ctx) {
  *out_ctx = new kcounter::VmoToInspectMapper;
  return ZX_OK;
}

void Release(void* ctx) { delete static_cast<kcounter::VmoToInspectMapper*>(ctx); }

constexpr const char* kKcounterServices[] = {
    fidl::DiscoverableProtocolName<fuchsia_kernel::Counter>,
    nullptr,
};

constexpr zx_service_ops_t kKcounterOps = {
    .init = Init,
    .connect = Connect,
    .release = Release,
};

constexpr zx_service_provider_t kcounter_service_provider = {
    .version = SERVICE_PROVIDER_VERSION,
    .services = kKcounterServices,
    .ops = &kKcounterOps,
};

}  // namespace

const zx_service_provider_t* kcounter_get_service_provider() { return &kcounter_service_provider; }
