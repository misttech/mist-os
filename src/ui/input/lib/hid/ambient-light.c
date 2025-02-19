// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/hid/ambient-light.h>
#include <lib/hid/descriptor.h>

// clang-format off

#define HID_USAGE_AMBIENT_LIGHT  HID_USAGE16(0x04D1)
#define HID_USAGE_INFRARED_LIGHT HID_USAGE16(0x04D7)
#define HID_USAGE_RED_LIGHT      HID_USAGE16(0x04D8)
#define HID_USAGE_GREEN_LIGHT    HID_USAGE16(0x04D9)
#define HID_USAGE_BLUE_LIGHT     HID_USAGE16(0x04DA)

static const uint8_t ambient_light_report_desc[] = {
    HID_USAGE_PAGE(0x20), // Sensor
    HID_USAGE(0x41), // Ambient Light
    HID_COLLECTION_APPLICATION,


    // Feature reports
    HID_REPORT_ID(AMBIENT_LIGHT_RPT_ID_FEATURE),

    HID_USAGE_SENSOR_PROPERTY_REPORTING_STATE,
    HID_LOGICAL_MIN(0),
    HID_LOGICAL_MAX(5),
    HID_REPORT_SIZE(8),
    HID_REPORT_COUNT(1),
    HID_COLLECTION_LOGICAL,
        HID_USAGE_SENSOR_PROPERTY_REPORTING_STATE_NO_EVENTS,
        HID_USAGE_SENSOR_PROPERTY_REPORTING_STATE_ALL_EVENTS,
        HID_USAGE_SENSOR_PROPERTY_REPORTING_STATE_THRESHOLD_EVENTS,
        HID_USAGE_SENSOR_PROPERTY_REPORTING_STATE_NO_EVENTS_WAKE,
        HID_USAGE_SENSOR_PROPERTY_REPORTING_STATE_ALL_EVENTS_WAKE,
        HID_USAGE_SENSOR_PROPERTY_REPORTING_STATE_THRESHOLD_EVENTS_WAKE,
        HID_FEATURE(HID_Data_Arr_Abs),
    HID_END_COLLECTION,

    HID_USAGE_SENSOR_PROPERTY_REPORT_INTERVAL,
    HID_LOGICAL_MIN(0),
    HID_LOGICAL_MAX32(0x7FFFFFFF),
    HID_REPORT_SIZE(32),
    HID_REPORT_COUNT(1),
    // Default is HID_USAGE_SENSOR_UNITS_MILLISECOND,
    HID_UNIT_EXPONENT(0),
    HID_FEATURE(HID_Data_Var_Abs),

    HID_USAGE_SENSOR_DATA(HID_USAGE_AMBIENT_LIGHT, HID_USAGE_SENSOR_DATA_MOD_THRESHOLD_LOW),
    HID_LOGICAL_MIN(0x00),
    HID_LOGICAL_MAX32(0xFFFF),
    HID_REPORT_SIZE(16),
    HID_REPORT_COUNT(1),
    HID_USAGE_SENSOR_GENERIC_UNITS_LUX,
    HID_FEATURE(HID_Data_Var_Abs),

    HID_USAGE_SENSOR_DATA(HID_USAGE_AMBIENT_LIGHT, HID_USAGE_SENSOR_DATA_MOD_THRESHOLD_HIGH),
    HID_LOGICAL_MIN(0x00),
    HID_LOGICAL_MAX32(0xFFFF),
    HID_REPORT_SIZE(16),
    HID_REPORT_COUNT(1),
    HID_USAGE_SENSOR_GENERIC_UNITS_LUX,
    HID_FEATURE(HID_Data_Var_Abs),


    // Input reports
    HID_REPORT_ID(AMBIENT_LIGHT_RPT_ID_INPUT),

    HID_USAGE_SENSOR_STATE,
    HID_LOGICAL_MIN(0),
    HID_LOGICAL_MAX(6),
    HID_REPORT_SIZE(8),
    HID_REPORT_COUNT(1),
    HID_COLLECTION_LOGICAL,
        HID_USAGE_SENSOR_STATE_UNKNOWN,
        HID_USAGE_SENSOR_STATE_READY,
        HID_USAGE_SENSOR_STATE_NOT_AVAILABLE,
        HID_USAGE_SENSOR_STATE_NO_DATA,
        HID_USAGE_SENSOR_STATE_INITIALIZING,
        HID_USAGE_SENSOR_STATE_ACCESS_DENIED,
        HID_USAGE_SENSOR_STATE_ERROR,
        HID_INPUT(HID_Const_Arr_Abs),
    HID_END_COLLECTION,

    HID_USAGE_SENSOR_EVENT,
    HID_LOGICAL_MIN(0),
    HID_LOGICAL_MAX(16),
    HID_REPORT_SIZE(8),
    HID_REPORT_COUNT(1),
    HID_COLLECTION_LOGICAL,
        HID_USAGE_SENSOR_EVENT_UNKNOWN,
        HID_USAGE_SENSOR_EVENT_STATE_CHANGED,
        HID_USAGE_SENSOR_EVENT_PROPERTY_CHANGED,
        HID_USAGE_SENSOR_EVENT_DATA_UPDATED,
        HID_USAGE_SENSOR_EVENT_POLL_RESPONSE,
        HID_USAGE_SENSOR_EVENT_CHANGE_SENSITIVITY,
        HID_USAGE_SENSOR_EVENT_MAX_REACHED,
        HID_USAGE_SENSOR_EVENT_MIN_REACHED,
        HID_USAGE_SENSOR_EVENT_HIGH_THRESHOLD_CROSS_UPWARD,
        HID_USAGE_SENSOR_EVENT_HIGH_THRESHOLD_CROSS_DOWNWARD,
        HID_USAGE_SENSOR_EVENT_LOW_THRESHOLD_CROSS_UPWARD,
        HID_USAGE_SENSOR_EVENT_LOW_THRESHOLD_CROSS_DOWNWARD,
        HID_USAGE_SENSOR_EVENT_ZERO_THRESHOLD_CROSS_UPWARD,
        HID_USAGE_SENSOR_EVENT_ZERO_THRESHOLD_CROSS_DOWNWARD,
        HID_USAGE_SENSOR_EVENT_PERIOD_EXCEEDED,
        HID_USAGE_SENSOR_EVENT_FREQUENCY_EXCEEDED,
        HID_USAGE_SENSOR_EVENT_COMPLEX_TRIGGER,
        HID_INPUT(HID_Const_Arr_Abs),
    HID_END_COLLECTION,

    HID_USAGE_AMBIENT_LIGHT,
    HID_LOGICAL_MIN(0),
    HID_LOGICAL_MAX32(0xFFFF),
    HID_REPORT_SIZE(16),
    HID_REPORT_COUNT(1),
    HID_USAGE_SENSOR_GENERIC_UNITS_NOT_SPECIFIED, // Explicitly not Lux
    HID_INPUT(HID_Data_Var_Abs),

    HID_USAGE_RED_LIGHT,
    HID_LOGICAL_MIN(0),
    HID_LOGICAL_MAX32(0xFFFF),
    HID_REPORT_SIZE(16),
    HID_REPORT_COUNT(1),
    HID_USAGE_SENSOR_GENERIC_UNITS_NOT_SPECIFIED, // Explicitly not Lux
    HID_INPUT(HID_Data_Var_Abs),

    HID_USAGE_BLUE_LIGHT,
    HID_LOGICAL_MIN(0),
    HID_LOGICAL_MAX32(0xFFFF),
    HID_REPORT_SIZE(16),
    HID_REPORT_COUNT(1),
    HID_USAGE_SENSOR_GENERIC_UNITS_NOT_SPECIFIED, // Explicitly not Lux
    HID_INPUT(HID_Data_Var_Abs),

    HID_USAGE_GREEN_LIGHT,
    HID_LOGICAL_MIN(0),
    HID_LOGICAL_MAX32(0xFFFF),
    HID_REPORT_SIZE(16),
    HID_REPORT_COUNT(1),
    HID_USAGE_SENSOR_GENERIC_UNITS_NOT_SPECIFIED, // Explicitly not Lux
    HID_INPUT(HID_Data_Var_Abs),


    HID_END_COLLECTION,
};
// clang-format on

size_t get_ambient_light_report_desc(const uint8_t** buf) {
  *buf = ambient_light_report_desc;
  return sizeof(ambient_light_report_desc);
}
