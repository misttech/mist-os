// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_ADC_DRIVERS_AML_SARADC_REGISTERS_H_
#define SRC_DEVICES_ADC_DRIVERS_AML_SARADC_REGISTERS_H_

#include <cstdint>

// clang-format off

// saradc registers
static constexpr uint32_t AO_SAR_ADC_REG0_OFFS           = (0x00 << 2);
static constexpr uint32_t AO_SAR_ADC_CHAN_LIST_OFFS      = (0x01 << 2);
static constexpr uint32_t AO_SAR_ADC_AVG_CNTL_OFFS       = (0x02 << 2);
static constexpr uint32_t AO_SAR_ADC_REG3_OFFS           = (0x03 << 2);
static constexpr uint32_t AO_SAR_ADC_DELAY_OFFS          = (0x04 << 2);
static constexpr uint32_t AO_SAR_ADC_LAST_RD_OFFS        = (0x05 << 2);
static constexpr uint32_t AO_SAR_ADC_FIFO_RD_OFFS        = (0x06 << 2);
static constexpr uint32_t AO_SAR_ADC_AUX_SW_OFFS         = (0x07 << 2);
static constexpr uint32_t AO_SAR_ADC_CHAN_10_SW_OFFS     = (0x08 << 2);
static constexpr uint32_t AO_SAR_ADC_DETECT_IDLE_SW_OFFS = (0x09 << 2);
static constexpr uint32_t AO_SAR_ADC_DELTA_10_OFFS       = (0x0a << 2);
static constexpr uint32_t AO_SAR_ADC_REG11_OFFS          = (0x0b << 2);
static constexpr uint32_t AO_SAR_ADC_REG13_OFFS          = (0x0d << 2);
static constexpr uint32_t AO_SAR_ADC_CHNL01_OFFS         = (0x0e << 2);
static constexpr uint32_t AO_SAR_ADC_CHNL23_OFFS         = (0x0f << 2);
static constexpr uint32_t AO_SAR_ADC_CHNL45_OFFS         = (0x10 << 2);
static constexpr uint32_t AO_SAR_ADC_CHNL67_OFFS         = (0x11 << 2);

// reg0 bit definitions
static constexpr uint32_t REG0_SAMPLING_STOP_MASK  = (0x01 << 14);
static constexpr uint32_t REG0_FIFO_CNT_IRQ_POS  = 4;
static constexpr uint32_t REG0_FIFO_CNT_IRQ_MASK  = (0x1F << REG0_FIFO_CNT_IRQ_POS);
static constexpr uint32_t REG0_FIFO_IRQ_EN_MASK  = (0x01 << 3);
static constexpr uint32_t REG0_SAMPLING_START_MASK  = (0x01 << 2);
static constexpr uint32_t REG0_SAMPLING_ENABLE_MASK  = (0x01 << 0);

// reg3 bit definitions
static constexpr uint32_t REG3_ADC_EN_MASK  = (0x01 << 21);

// reg11 bit definitions
static constexpr uint32_t REG11_TS_VBG_EN_MASK  = (0x01 << 13);
static constexpr uint32_t REG11_RSV6_MASK  = (0x01 << 6);
static constexpr uint32_t REG11_RSV5_MASK  = (0x01 << 5);
static constexpr uint32_t REG11_RSV1_MASK  = (0x01 << 1);

// saradc clock control register
static constexpr uint32_t AO_SAR_CLK_OFFS                = (0x24 << 2);
// saradc clock control register bit definitions
static constexpr uint32_t AO_SAR_CLK_ENA_POS             = 8;
static constexpr uint32_t AO_SAR_CLK_ENA_MASK            = (0x01 << AO_SAR_CLK_ENA_POS);
static constexpr uint32_t AO_SAR_CLK_SRC_POS             = 9;
static constexpr uint32_t AO_SAR_CLK_SRC_MASK            = (0x03 << AO_SAR_CLK_SRC_POS);
static constexpr uint32_t AO_SAR_CLK_DIV_POS             = 0;
static constexpr uint32_t AO_SAR_CLK_DIV_MASK            = (0xff << AO_SAR_CLK_DIV_POS);

// clang-format on

#endif  // SRC_DEVICES_ADC_DRIVERS_AML_SARADC_REGISTERS_H_
