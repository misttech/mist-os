// Copyright (c) 2019 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <memory>

#if CONFIG_BRCMFMAC_SDIO
#include <lib/ddk/binding_driver.h>  // nogncheck

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sdio/sdio_device.h"  // nogncheck
#endif  // CONFIG_BRCMFMAC_SDIO

static constexpr zx_driver_ops_t brcmfmac_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind =
        [](void* ctx, zx_device_t* device) {
          zx_status_t status = ZX_ERR_NOT_SUPPORTED;
#if CONFIG_BRCMFMAC_SDIO
          status = ::wlan::brcmfmac::SdioDevice::Create(device);
#endif  // CONFIG_BRCMFMAC_SDIO
          return status;
        },
};

ZIRCON_DRIVER(brcmfmac, brcmfmac_driver_ops, "zircon", "0.1");
