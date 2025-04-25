// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/devmgr/coordinator.h"

#include <lib/ddk/driver.h>
#include <lib/mistos/devmgr/driver.h>
#include <trace.h>

#include <algorithm>

#include <bind/fuchsia/cpp/bind.h>
#include <fbl/auto_lock.h>
#include <misc/drivers/mistos/device.h>
#include <misc/drivers/mistos/driver.h>
#include <misc/drivers/mistos/symbols.h>

#define LOCAL_TRACE 0

namespace devmgr {

namespace {

struct BindProgramContext {
  fbl::Vector<zx_device_str_prop>* props;
  uint32_t protocol_id;
  uint32_t binding_size;
  const zx_bind_inst_t* binding;
  const char* name;
  uint32_t autobind;
};

std::string_view bind_id_to_string(uint32_t id) {
  switch (id) {
    case BIND_PROTOCOL:
      return bind_fuchsia::PROTOCOL;
    case BIND_AUTOBIND:
      return bind_fuchsia::AUTOBIND;
    case BIND_PCI_VID:
      return bind_fuchsia::PCI_VID;
    case BIND_PCI_DID:
      return bind_fuchsia::PCI_DID;
    case BIND_PCI_CLASS:
      return bind_fuchsia::PCI_CLASS;
    case BIND_PCI_SUBCLASS:
      return bind_fuchsia::PCI_SUBCLASS;
    case BIND_PLATFORM_DEV_VID:
      return bind_fuchsia::PLATFORM_DEV_VID;
    case BIND_PLATFORM_DEV_PID:
      return bind_fuchsia::PLATFORM_DEV_PID;
    case BIND_PLATFORM_DEV_DID:
      return bind_fuchsia::PLATFORM_DEV_DID;
    // case BIND_PLATFORM_PROTO:
    // return bind_fuchsia::PLATFORM_PROTO;
    default:
      return "UNKNOWN";
  }
}

uint32_t dev_get_prop(BindProgramContext* ctx, uint32_t id) {
  fbl::Vector<zx_device_str_prop>* props = ctx->props;

  // Convert the numeric ID to a string key
  std::string_view key = bind_id_to_string(id);

  LTRACEF("id=%d key=%.*s\n", id, static_cast<int>(key.size()), key.data());

  // Search for a property with matching key
  auto it = std::find_if(props->begin(), props->end(),
                         [&key](const zx_device_str_prop& prop) { return prop.key == key; });

  // If found, return the integer value
  if (it != props->end() && it->property_value.data_type == ZX_DEVICE_PROPERTY_VALUE_INT) {
    return it->property_value.data.int_val;
  }

#if 0
  // fallback for devices without properties
  switch (id) {
    case BIND_PROTOCOL:
      return ctx->protocol_id;
    case BIND_AUTOBIND:
      return ctx->autobind;
    default:
      // TODO: better process for missing properties
      return 0;
  }
#endif

  // TODO: better process for missing properties
  return 0;
}

bool is_bindable(BindProgramContext* ctx) {
  const zx_bind_inst_t* ip = ctx->binding;
  const zx_bind_inst_t* end = ip + (ctx->binding_size / sizeof(zx_bind_inst_t));
  uint32_t flags = 0;

  while (ip < end) {
    uint32_t inst = ip->op;
    bool cond;

    if (BINDINST_CC(inst) != COND_AL) {
      uint32_t value = ip->arg;
      uint32_t pid = BINDINST_PB(inst);
      uint32_t pval;
      if (pid != BIND_FLAGS) {
        pval = dev_get_prop(ctx, pid);
      } else {
        pval = flags;
      }

      // evaluate condition
      switch (BINDINST_CC(inst)) {
        case COND_EQ:
          cond = (pval == value);
          break;
        case COND_NE:
          cond = (pval != value);
          break;
        case COND_LT:
          cond = (pval < value);
          break;
        case COND_GT:
          cond = (pval > value);
          break;
        case COND_LE:
          cond = (pval <= value);
          break;
        case COND_GE:
          cond = (pval >= value);
          break;
        default:
          // illegal instruction: abort
          printf("devmgr: driver '%s' illegal bindinst 0x%08x\n", ctx->name, inst);
          return false;
      }
    } else {
      cond = true;
    }

    if (cond) {
      switch (BINDINST_OP(inst)) {
        case OP_ABORT:
          return false;
        case OP_MATCH:
          return true;
        case OP_GOTO: {
          uint32_t label = BINDINST_PA(inst);
          while (++ip < end) {
            if ((BINDINST_OP(ip->op) == OP_LABEL) && (BINDINST_PA(ip->op) == label)) {
              goto next_instruction;
            }
          }
          printf("devmgr: driver '%s' illegal GOTO\n", ctx->name);
          return false;
        }
        case OP_LABEL:
          // no op
          break;
        default:
          // illegal instruction: abort
          printf("devmgr: driver '%s' illegal bindinst 0x%08x\n", ctx->name, inst);
          return false;
      }
    }

  next_instruction:
    ip++;
  }

  // default if we leave the program is no-match
  return false;
}

bool dc_is_bindable(const Driver* drv, fbl::Vector<zx_device_str_prop>& props, bool autobind) {
  if (drv->binding_size() == 0) {
    return false;
  }

  BindProgramContext ctx;
  // ctx.protocol_id = 0;

  // Find protocol ID in properties using find_if
  // auto it = std::ranges::find_if(
  //    props, [](const auto& prop) { return prop.key == bind_fuchsia::PROTOCOL; });

  // if (it != props.end() && it->property_value.data_type == ZX_DEVICE_PROPERTY_VALUE_INT) {
  //   ctx.protocol_id = it->property_value.data.int_val;
  // }

  ctx.props = &props;
  ctx.binding = drv->binding();
  ctx.binding_size = drv->binding_size();
  ctx.name = drv->url().data();
  // ctx.autobind = autobind ? 1 : 0;
  return is_bindable(&ctx);
}

}  // namespace

void Coordinator::DriverAdded(fbl::RefPtr<Driver> drv, const char* version) {
  // fbl::AutoLock lock(&mutex_);
  drivers_.push_back(drv);
}

void Coordinator::DriverAddedInit(fbl::RefPtr<Driver> drv, const char* version) {
  // fbl::AutoLock lock(&mutex_);
  drivers_.push_back(drv);
}

zx::result<> Coordinator::StartRootDriver(std::string_view name) {
  return StartDriver(root_device_, nullptr, name);
}

zx::result<> Coordinator::StartDriver(mistos::device_t device, const zx_protocol_device_t* ops,
                                      std::string_view name) {
  // fbl::AutoLock lock(&mutex_);
  for (auto& drv : drivers_) {
    if (drv.url() == name) {
      fbl::AllocChecker ac;
      mistos::DriverStartArgs start_args = {.driver_base = drv.library_};
      auto driver = ktl::make_unique<mistos::Driver>(&ac, start_args, device, ops);
      if (!ac.check()) {
        return zx::error(ZX_ERR_NO_MEMORY);
      }

      driver->Start([](zx::result<> result) {
        if (result.is_error()) {
          LTRACEF("Failed to start driver: %d\n", result.error_value());
        }
      });

      // TODO (Herrera) Fix leak
      driver.release();

      return zx::ok();
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}  // namespace devmgr

void Coordinator::HandleNewDevice(mistos::Device* dev) {
  // fbl::AutoLock lock(&mutex_);
  for (auto& drv : drivers_) {
    // uint32_t proto_id = dev->GetProtocol(ZX_PROTOCOL_MISC, nullptr);
    if (!dc_is_bindable(&drv, dev->properties(), true)) {
      continue;
    }
    LTRACEF("devcoord: drv='%.*s' bindable to dev='%s'\n", static_cast<int>(drv.url().size()),
            drv.url().data(), dev->Name());
    zx_status_t status = AttemptBind(&drv, dev);
    if (status != ZX_OK) {
      LTRACEF("failed to bind drv='%.*s' to dev='%s': %d\n", static_cast<int>(drv.url().size()),
              drv.url().data(), dev->Name(), status);
    }
    // if (!(dev->flags & DEV_CTX_MULTI_BIND)) {
    //   break;
    // }
  }
}

zx_status_t Coordinator::AttemptBind(const Driver* drv, mistos::Device* dev) {
  mistos::device_t device{
      .name = dev->Name(),
      .context = (void*)dev->ZxDevice(),
  };

  if (auto result = StartDriver(device, nullptr, drv->url()); result.is_error()) {
    LTRACEF("Failed to start driver: %d\n", result.error_value());
    return result.error_value();
  }

  return ZX_OK;
}

void Coordinator::DumpState() {
  // fbl::AutoLock lock(&mutex_);
}

namespace {

const char* di_bind_param_name(uint32_t param_num) {
  switch (param_num) {
    case BIND_FLAGS:
      return "Flags";
    case BIND_PROTOCOL:
      return "Protocol";
    case BIND_AUTOBIND:
      return "Autobind";
    case BIND_PCI_VID:
      return "PCI.VID";
    case BIND_PCI_DID:
      return "PCI.DID";
    case BIND_PCI_CLASS:
      return "PCI.Class";
    case BIND_PCI_SUBCLASS:
      return "PCI.Subclass";
    case BIND_PCI_INTERFACE:
      return "PCI.Interface";
    case BIND_PCI_REVISION:
      return "PCI.Revision";
    case BIND_PCI_BDF_ADDR:
      return "PCI.BDFAddr";
    case BIND_USB_VID:
      return "USB.VID";
    case BIND_USB_PID:
      return "USB.PID";
    case BIND_USB_CLASS:
      return "USB.Class";
    case BIND_USB_SUBCLASS:
      return "USB.Subclass";
    case BIND_USB_PROTOCOL:
      return "USB.Protocol";
    case BIND_PLATFORM_DEV_VID:
      return "PlatDev.VID";
    case BIND_PLATFORM_DEV_PID:
      return "PlatDev.PID";
    case BIND_PLATFORM_DEV_DID:
      return "PlatDev.DID";
    case BIND_ACPI_HID_0_3:
      return "ACPI.HID[0-3]";
    case BIND_ACPI_HID_4_7:
      return "ACPI.HID[4-7]";
    case BIND_IHDA_CODEC_VID:
      return "IHDA.Codec.VID";
    case BIND_IHDA_CODEC_DID:
      return "IHDA.Codec.DID";
    case BIND_IHDA_CODEC_MAJOR_REV:
      return "IHDACodec.MajorRev";
    case BIND_IHDA_CODEC_MINOR_REV:
      return "IHDACodec.MinorRev";
    case BIND_IHDA_CODEC_VENDOR_REV:
      return "IHDACodec.VendorRev";
    case BIND_IHDA_CODEC_VENDOR_STEP:
      return "IHDACodec.VendorStep";
    default:
      return NULL;
  }
}

void di_dump_bind_inst(const zx_bind_inst_t* b, char* buf, size_t buf_len) {
  if (!b || !buf || !buf_len) {
    return;
  }

  uint32_t cc = BINDINST_CC(b->op);
  uint32_t op = BINDINST_OP(b->op);
  uint32_t pa = BINDINST_PA(b->op);
  uint32_t pb = BINDINST_PB(b->op);
  size_t off = 0;
  buf[0] = 0;

  switch (op) {
    case OP_ABORT:
    case OP_MATCH:
    case OP_GOTO:
      break;
    case OP_LABEL:
      snprintf(buf + off, buf_len - off, "L.%u:", pa);
      return;
    default:
      snprintf(buf + off, buf_len - off, "Unknown Op 0x%1x [0x%08x, 0x%08x]", op, b->op, b->arg);
      return;
  }

  off += snprintf(buf + off, buf_len - off, "if (");
  if (cc == COND_AL) {
    off += snprintf(buf + off, buf_len - off, "true");
  } else {
    const char* pb_name = di_bind_param_name(pb);
    if (pb_name) {
      off += snprintf(buf + off, buf_len - off, "%s", pb_name);
    } else {
      off += snprintf(buf + off, buf_len - off, "P.%04x", pb);
    }

    switch (cc) {
      case COND_EQ:
        off += snprintf(buf + off, buf_len - off, " == 0x%08x", b->arg);
        break;
      case COND_NE:
        off += snprintf(buf + off, buf_len - off, " != 0x%08x", b->arg);
        break;
      case COND_GT:
        off += snprintf(buf + off, buf_len - off, " > 0x%08x", b->arg);
        break;
      case COND_LT:
        off += snprintf(buf + off, buf_len - off, " < 0x%08x", b->arg);
        break;
      case COND_GE:
        off += snprintf(buf + off, buf_len - off, " >= 0x%08x", b->arg);
        break;
      case COND_LE:
        off += snprintf(buf + off, buf_len - off, " <= 0x%08x", b->arg);
        break;
      default:
        off += snprintf(buf + off, buf_len - off, " ?(0x%x) 0x%08x", cc, b->arg);
        break;
    }
  }
  off += snprintf(buf + off, buf_len - off, ") ");

  switch (op) {
    case OP_ABORT:
      off += snprintf(buf + off, buf_len - off, "return no-match;");
      break;
    case OP_MATCH:
      off += snprintf(buf + off, buf_len - off, "return match;");
      break;
    case OP_GOTO:
      off += snprintf(buf + off, buf_len - off, "goto L.%u;", b->arg);
      break;
  }
}

}  // namespace

void Coordinator::DumpDrivers() {
  // fbl::AutoLock lock(&mutex_);
  bool first = true;
  for (auto& drv : drivers_) {
    // fbl::AutoLock drv_lock(&drv.lock_);
    printf("%sName    : %.*s\n", first ? "" : "\n", static_cast<int>(drv.url().size()),
           drv.url().data());
    // printf("Driver  : %.*s\n", static_cast<int>(drv.driver_name_.size()),
    // drv.driver_name_.data());
    printf("Flags   : 0x%08x\n", drv.flags_);
    if (drv.binding_.has_value()) {
      uint32_t count = drv.binding_size_ / static_cast<uint32_t>(sizeof(drv.binding_->get()[0]));
      printf("Binding : %u instruction%s (%u bytes)\n", count, (count == 1) ? "" : "s",
             drv.binding_size_);
      char line[256];
      for (uint32_t i = 0; i < count; ++i) {
        di_dump_bind_inst(&drv.binding_->get()[i], line, sizeof(line));
        printf("[%u/%u]: %s\n", i + 1, count, line);
      }
    }
    first = false;
  }
}

}  // namespace devmgr
