// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_UIC_UIC_COMMANDS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_UIC_UIC_COMMANDS_H_

namespace ufs {

// MIPI UniPro specification v2.0, section 5.8.1 "PHY Adapter Common Attributes"
#define PA_AvailTxDataLanes 0x1520
#define PA_AvailRxDataLanes 0x1540

// MIPI UniPro specification v2.0, section 5.8.2 "PHY Adapter M-PHY-Specific Attributes"
#define PA_ConnectedTxDataLanes 0x1561
#define PA_ConnectedRxDataLanes 0x1581
#define PA_MaxRxHSGear 0x1587

constexpr uint32_t kUicTimeoutUsec = 5000000;

enum class UicCommandOpcode {
  // Configuration
  kDmeGet = 0x01,
  kDmeSet = 0x02,
  kDmePeerGet = 0x03,
  kDmePeerSet = 0x04,
  // Control
  kDmePowerOn = 0x10,
  kDmePowerOff = 0x11,
  kDmeEnable = 0x12,
  kDmeReset = 0x14,
  kDmeEndpointReset = 0x15,
  kDmeLinkStartUp = 0x16,
  kDmeHibernateEnter = 0x17,
  kDmeHibernateExit = 0x18,
  kDmeTestMode = 0x1a,
};

class Ufs;

// UFS Specification Version 3.1, section 9.4 "UniPro/UFS Control Interface (Control Plane)".
class UicCommand {
 public:
  explicit UicCommand(Ufs &ufs, UicCommandOpcode opcode, uint16_t mbi_attribute,
                      uint16_t gen_selector_index)
      : controller_(ufs),
        opcode_(opcode),
        mbi_attribute_(mbi_attribute),
        gen_selector_index_(gen_selector_index) {}

  virtual zx::result<std::optional<uint32_t>> SendCommand() = 0;

  // For testing
  void SetTimeoutUsec(uint32_t time) { timeout_usec_ = time; }

 protected:
  zx::result<> SendUicCommand(uint32_t argument1, uint32_t argument2, uint32_t argument3);

  const Ufs &GetController() const { return controller_; }
  UicCommandOpcode GetOpcode() const { return opcode_; }
  uint16_t GetMbiAttribute() const { return mbi_attribute_; }
  uint16_t GetGenSelectorIndex() const { return gen_selector_index_; }

 private:
  const Ufs &controller_;
  const UicCommandOpcode opcode_;
  const uint16_t mbi_attribute_ = 0;
  const uint16_t gen_selector_index_ = 0;

  uint32_t timeout_usec_ = kUicTimeoutUsec;
};

class DmeGetUicCommand : public UicCommand {
 public:
  explicit DmeGetUicCommand(Ufs &ufs, uint16_t mbi_attribute, uint16_t gen_selector_index)
      : UicCommand(ufs, UicCommandOpcode::kDmeGet, mbi_attribute, gen_selector_index) {}
  zx::result<std::optional<uint32_t>> SendCommand() override;
};

class DmeSetUicCommand : public UicCommand {
 public:
  explicit DmeSetUicCommand(Ufs &ufs, uint16_t mbi_attribute, uint16_t gen_selector_index,
                            uint32_t value)
      : UicCommand(ufs, UicCommandOpcode::kDmeSet, mbi_attribute, gen_selector_index),
        value_(value) {}
  zx::result<std::optional<uint32_t>> SendCommand() override;

 private:
  uint32_t value_;
};

class DmeLinkStartUpUicCommand : public UicCommand {
 public:
  explicit DmeLinkStartUpUicCommand(Ufs &ufs)
      : UicCommand(ufs, UicCommandOpcode::kDmeLinkStartUp, 0, 0) {}
  zx::result<std::optional<uint32_t>> SendCommand() override;
};

class DmeHibernateEnterCommand : public UicCommand {
 public:
  explicit DmeHibernateEnterCommand(Ufs &ufs)
      : UicCommand(ufs, UicCommandOpcode::kDmeHibernateEnter, 0, 0) {}
  zx::result<std::optional<uint32_t>> SendCommand() override;
};

class DmeHibernateExitCommand : public UicCommand {
 public:
  explicit DmeHibernateExitCommand(Ufs &ufs)
      : UicCommand(ufs, UicCommandOpcode::kDmeHibernateExit, 0, 0) {}
  zx::result<std::optional<uint32_t>> SendCommand() override;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_UIC_UIC_COMMANDS_H_
