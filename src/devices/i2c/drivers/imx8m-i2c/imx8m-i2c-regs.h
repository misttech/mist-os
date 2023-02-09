// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_IMX8M_I2C_IMX8M_I2C_REGS_H_
#define SRC_DEVICES_I2C_DRIVERS_IMX8M_I2C_IMX8M_I2C_REGS_H_

#include <hwreg/bitfields.h>

namespace imx8m_i2c {

constexpr size_t kRegisterSetSize = 20;

// I2C Address Register (I2C_IADR)
class AddressReg : public hwreg::RegisterBase<AddressReg, uint16_t> {
 public:
  DEF_RSVDZ_FIELD(15, 8);
  DEF_FIELD(7, 1, adr);
  DEF_RSVDZ_BIT(0);
  static auto Get() { return hwreg::RegisterAddr<AddressReg>(0x00); }
};

// I2C Frequency Divider Register (I2C_IFDR)
class FrequencyDividerRegister : public hwreg::RegisterBase<FrequencyDividerRegister, uint16_t> {
 public:
  DEF_RSVDZ_FIELD(15, 6);
  DEF_FIELD(5, 0, ic);
  static auto Get() { return hwreg::RegisterAddr<FrequencyDividerRegister>(0x04); }
};

// I2C Control Register (I2C_I2CR)
class ControlReg : public hwreg::RegisterBase<ControlReg, uint16_t> {
 public:
  DEF_RSVDZ_FIELD(15, 8);
  DEF_BIT(7, ien);
  DEF_BIT(6, iien);
  DEF_BIT(5, msta);
  DEF_BIT(4, mtx);
  DEF_BIT(3, txak);
  DEF_BIT(2, rsta);
  DEF_RSVDZ_FIELD(1, 0);
  static auto Get() { return hwreg::RegisterAddr<ControlReg>(0x08); }
};

// I2C Status Register (I2C_I2SR)
class StatusReg : public hwreg::RegisterBase<StatusReg, uint16_t> {
 public:
  DEF_RSVDZ_FIELD(15, 8);
  DEF_BIT(7, icf);
  DEF_BIT(6, iaas);
  DEF_BIT(5, ibb);
  DEF_BIT(4, ial);
  DEF_RSVDZ_BIT(3);
  DEF_BIT(2, srw);
  DEF_BIT(1, iif);
  DEF_BIT(0, rxak);
  static auto Get() { return hwreg::RegisterAddr<StatusReg>(0x0c); }
};

// I2C Data I/O Register (I2C_I2DR)
class DataReg : public hwreg::RegisterBase<DataReg, uint16_t> {
 public:
  DEF_RSVDZ_FIELD(15, 8);
  DEF_FIELD(7, 0, data);
  static auto Get() { return hwreg::RegisterAddr<DataReg>(0x10); }
};

}  // namespace imx8m_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_IMX8M_I2C_IMX8M_I2C_REGS_H_
