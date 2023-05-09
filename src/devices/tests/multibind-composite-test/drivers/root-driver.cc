// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/tests/multibind-composite-test/drivers/root-driver.h"

#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/test/cpp/bind.h>
#include <bind/multibind/test/cpp/bind.h>

#include "src/devices/tests/multibind-composite-test/drivers/root-driver-bind.h"

namespace root_driver {

namespace {

// Node a bind rules and properties.
const zx_bind_inst_t kNodeAMatch[] = {
    BI_ABORT_IF(NE, BIND_PROTOCOL, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD),
    BI_MATCH_IF(EQ, BIND_PLATFORM_DEV_INSTANCE_ID, 1),
};
const device_fragment_part_t kNodeAFragment[] = {
    {std::size(kNodeAMatch), kNodeAMatch},
};

const ddk::BindRule kNodeARules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD),
    ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, static_cast<uint32_t>(1)),
};

const device_bind_prop_t kNodeAProperties[] = {
    ddk::MakeProperty(bind_multibind_test::NODE_ID, bind_multibind_test::NODE_ID_A),
};

// Node b bind rules and properties.
const zx_bind_inst_t kNodeBMatch[] = {
    BI_ABORT_IF(NE, BIND_PROTOCOL, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD),
    BI_MATCH_IF(EQ, BIND_PLATFORM_DEV_INSTANCE_ID, 2),
};
const device_fragment_part_t kNodeBFragment[] = {
    {std::size(kNodeBMatch), kNodeBMatch},
};

const ddk::BindRule kNodeBRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD),
    ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, static_cast<uint32_t>(2)),
};

const device_bind_prop_t kNodeBProperties[] = {
    ddk::MakeProperty(bind_multibind_test::NODE_ID, bind_multibind_test::NODE_ID_B),
};

// Node c bind rules and properties.
const zx_bind_inst_t kNodeCMatch[] = {
    BI_ABORT_IF(NE, BIND_PROTOCOL, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD),
    BI_MATCH_IF(EQ, BIND_PLATFORM_DEV_INSTANCE_ID, 3),
};
const device_fragment_part_t kNodeCFragment[] = {
    {std::size(kNodeCMatch), kNodeCMatch},
};

const ddk::BindRule kNodeCRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD),
    ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, static_cast<uint32_t>(3)),
};

const device_bind_prop_t kNodeCProperties[] = {
    ddk::MakeProperty(bind_multibind_test::NODE_ID, bind_multibind_test::NODE_ID_C),
};

// Node d bind rules and properties.
const ddk::BindRule kNodeDRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD),
    ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, static_cast<uint32_t>(4)),
};

const device_bind_prop_t kNodeDProperties[] = {
    ddk::MakeProperty(bind_multibind_test::NODE_ID, bind_multibind_test::NODE_ID_D),
};

// Legacy composite properties.
const zx_device_prop_t kLegacyCompositeProperties[] = {
    {BIND_PROTOCOL, 0, bind_fuchsia_test::BIND_PROTOCOL_DEVICE},
};

}  // namespace

zx_status_t RootDriver::Bind(void* ctx, zx_device_t* dev) {
  auto root_dev = std::make_unique<RootDriver>(dev);
  zx_status_t status = root_dev->DdkAdd(ddk::DeviceAddArgs("root"));
  if (status != ZX_OK) {
    return status;
  }

  const std::string kNodes[] = {"node_a", "node_b", "node_c", "node_d"};
  uint32_t instance_id = 1;
  for (auto& node : kNodes) {
    zx_device_prop_t node_props[] = {
        {BIND_PROTOCOL, 0, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD},
        {BIND_PLATFORM_DEV_INSTANCE_ID, 0, instance_id++},
    };

    auto node_dev = std::make_unique<RootDriver>(dev);
    status = node_dev->DdkAdd(ddk::DeviceAddArgs(node.c_str())
                                  .set_props(node_props)
                                  .set_proto_id(bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD)
                                  .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE));
    if (status != ZX_OK) {
      return status;
    }
    [[maybe_unused]] auto node_dev_ptr = node_dev.release();
  }

  // Add the first legacy composite.
  const device_fragment_t kLegacyCompositeFragments1[] = {
      {"node-a", std::size(kNodeAFragment), kNodeAFragment},
      {"node-c", std::size(kNodeCFragment), kNodeCFragment},
  };

  const composite_device_desc_t kLegacyCompositeDesc1 = {
      .props = kLegacyCompositeProperties,
      .props_count = std::size(kLegacyCompositeProperties),
      .fragments = kLegacyCompositeFragments1,
      .fragments_count = std::size(kLegacyCompositeFragments1),
      .primary_fragment = "node-a",
      .spawn_colocated = true,
  };

  status = root_dev->DdkAddComposite("legacy_composite_1", &kLegacyCompositeDesc1);
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAddComposite failed: %d ", status);
    return status;
  }

  // Add composite node spec 1.
  status = root_dev->DdkAddCompositeNodeSpec("spec_1",
                                             ddk::CompositeNodeSpec(kNodeARules, kNodeAProperties)
                                                 .AddParentSpec(kNodeCRules, kNodeCProperties)
                                                 .AddParentSpec(kNodeDRules, kNodeDProperties));
  if (status != ZX_OK) {
    return status;
  }

  // Add second legacy composite.
  const device_fragment_t kLegacyCompositeFragments2[] = {
      {"node-b", std::size(kNodeBFragment), kNodeBFragment},
      {"node-c", std::size(kNodeCFragment), kNodeCFragment},
  };

  const composite_device_desc_t kLegacyCompositeDesc2 = {
      .props = kLegacyCompositeProperties,
      .props_count = std::size(kLegacyCompositeProperties),
      .fragments = kLegacyCompositeFragments2,
      .fragments_count = std::size(kLegacyCompositeFragments2),
      .primary_fragment = "node-b",
      .spawn_colocated = true,
  };

  status = root_dev->DdkAddComposite("legacy_composite_2", &kLegacyCompositeDesc2);
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAddComposite failed: %d ", status);
    return status;
  }

  // Add composite node spec 2.
  status = root_dev->DdkAddCompositeNodeSpec("spec_2",
                                             ddk::CompositeNodeSpec(kNodeDRules, kNodeDProperties)
                                                 .AddParentSpec(kNodeBRules, kNodeBProperties));
  if (status != ZX_OK) {
    return status;
  }

  [[maybe_unused]] auto ptr = root_dev.release();

  return ZX_OK;
}

void RootDriver::DdkRelease() { delete this; }

static zx_driver_ops_t root_driver_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = RootDriver::Bind;
  return ops;
}();

}  // namespace root_driver

ZIRCON_DRIVER(RootDriver, root_driver::root_driver_driver_ops, "zircon", "0.1");
