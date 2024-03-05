// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <ftw.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <set>
#include <string>
#include <vector>

#include <gtest/gtest.h>
#include <linux/input.h>
#include <linux/uinput.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

class UinputTest : public ::testing::Test {
 public:
  void SetUp() override {
    if (getuid() != 0) {
      GTEST_SKIP() << "Can only be run as root.";
    }

    uinput_fd_ = test_helper::ScopedFD(open("/dev/uinput", O_RDWR));
    ASSERT_TRUE(uinput_fd_.is_valid())
        << "open(\"/dev/uinput\") failed: " << strerror(errno) << "(" << errno << ")";
  }

 protected:
  test_helper::ScopedFD uinput_fd_;
};

std::set<std::string> lsDir(const char* dir) {
  DIR* d = opendir(dir);
  std::set<std::string> name_set;
  dirent* e;
  while ((e = readdir(d)) != nullptr) {
    name_set.insert(e->d_name);
  }
  closedir(d);
  return name_set;
}

// return files in the first ls result but not in the second ls result.
std::vector<std::string> lsDiff(std::set<std::string>& s1, std::set<std::string>& s2) {
  std::vector<std::string> diff;
  for (const auto& it : s1) {
    if (s2.find(it) == s2.end()) {
      diff.push_back(it);
    }
  }
  return diff;
}

const uint16_t GOOGLE_VENDOR_ID = 0x18d1;

TEST_F(UinputTest, UiGetVersion) {
  // Pass null to UI_GET_VERSION expect EFAULT.
  int res = ioctl(uinput_fd_.get(), UI_GET_VERSION, NULL);
  EXPECT_EQ(res, -1);
  EXPECT_EQ(errno, EFAULT);

  int version;
  res = ioctl(uinput_fd_.get(), UI_GET_VERSION, &version);
  EXPECT_EQ(res, 0);
  EXPECT_EQ(version, 5);
}

TEST_F(UinputTest, UiSetEvbit) {
  int res = ioctl(uinput_fd_.get(), UI_SET_EVBIT, EV_KEY);
  EXPECT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_SET_EVBIT, EV_ABS);
  EXPECT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_SET_EVBIT, EV_REL);
  EXPECT_EQ(res, -1);
}

TEST_F(UinputTest, UiSetKeybit) {
  int res = ioctl(uinput_fd_.get(), UI_SET_KEYBIT, KEY_SPACE);
  EXPECT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_SET_KEYBIT, KEY_A);
  EXPECT_EQ(res, 0);
}

TEST_F(UinputTest, UiSetPropbit) {
  int res = ioctl(uinput_fd_.get(), UI_SET_PROPBIT, INPUT_PROP_DIRECT);
  EXPECT_EQ(res, 0);
}

TEST_F(UinputTest, UiSetAbsbit) {
  int res = ioctl(uinput_fd_.get(), UI_SET_ABSBIT, ABS_MT_SLOT);
  EXPECT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_SET_ABSBIT, ABS_MT_TOUCH_MAJOR);
  EXPECT_EQ(res, 0);
}

TEST_F(UinputTest, UiSetPhys) {
  char name[] = "mouse0";
  int res = ioctl(uinput_fd_.get(), UI_SET_PHYS, &name);
  EXPECT_EQ(res, 0);
}

TEST_F(UinputTest, UiDevSetup) {
  uinput_setup usetup{.id = {.bustype = BUS_USB, .vendor = GOOGLE_VENDOR_ID, .product = 1}};
  strcpy(usetup.name, "Example device");

  int res = ioctl(uinput_fd_.get(), UI_DEV_SETUP, &usetup);
  EXPECT_EQ(res, 0);
}

TEST_F(UinputTest, UiDevSetupNull) {
  int res = ioctl(uinput_fd_.get(), UI_DEV_SETUP, NULL);
  EXPECT_EQ(res, -1);
  EXPECT_EQ(errno, EFAULT);
}

TEST_F(UinputTest, UiDevCreateFailedWithoutDevSetup) {
  int res = ioctl(uinput_fd_.get(), UI_DEV_CREATE);
  EXPECT_EQ(res, -1);
  EXPECT_EQ(errno, EINVAL);
}

TEST_F(UinputTest, UiDevCreateKeyboard) {
  uinput_setup usetup{.id = {.bustype = BUS_USB, .vendor = GOOGLE_VENDOR_ID, .product = 2}};
  strcpy(usetup.name, "Example device");
  int res = ioctl(uinput_fd_.get(), UI_DEV_SETUP, &usetup);
  ASSERT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_DEV_CREATE);
  EXPECT_EQ(res, 0);
}

TEST_F(UinputTest, UiDevCreateTouchscreen) {
  int res = ioctl(uinput_fd_.get(), UI_SET_EVBIT, EV_ABS);
  ASSERT_EQ(res, 0);

  uinput_setup usetup{.id = {.bustype = BUS_USB, .vendor = GOOGLE_VENDOR_ID, .product = 3}};
  strcpy(usetup.name, "Example device");
  res = ioctl(uinput_fd_.get(), UI_DEV_SETUP, &usetup);
  ASSERT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_DEV_CREATE);
  EXPECT_EQ(res, 0);
}

TEST_F(UinputTest, UiDevCreateDestroyTouchscreenEvIoGid) {
  auto ls_before = lsDir("/dev/input");

  int res = ioctl(uinput_fd_.get(), UI_SET_EVBIT, EV_ABS);
  ASSERT_EQ(res, 0);

  uinput_setup usetup{.id = {.bustype = BUS_USB, .vendor = GOOGLE_VENDOR_ID, .product = 4}};
  strcpy(usetup.name, "Example device");
  res = ioctl(uinput_fd_.get(), UI_DEV_SETUP, &usetup);
  ASSERT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_DEV_CREATE);
  ASSERT_EQ(res, 0);

  auto ls_after = lsDir("/dev/input");
  auto diff = lsDiff(ls_after, ls_before);
  ASSERT_EQ(diff.size(), 1u);

  auto new_device_name = diff[0];
  EXPECT_EQ(new_device_name.substr(0, std::string("event").length()), "event");

  {
    auto new_device_fd =
        test_helper::ScopedFD(open(("/dev/input/" + new_device_name).c_str(), O_RDWR));
    ASSERT_TRUE(new_device_fd.is_valid());
    input_id got_input_id;
    res = ioctl(new_device_fd.get(), EVIOCGID, &got_input_id);
    EXPECT_EQ(res, 0);
    EXPECT_EQ(got_input_id.bustype, BUS_USB);
    EXPECT_EQ(got_input_id.vendor, GOOGLE_VENDOR_ID);
    EXPECT_EQ(got_input_id.product, usetup.id.product);
  }

  res = ioctl(uinput_fd_.get(), UI_DEV_DESTROY);
  ASSERT_EQ(res, 0);

  // Check that the device file no longer exits. Open should fail.
  int fd = open(("/dev/input/" + new_device_name).c_str(), O_RDWR);
  EXPECT_EQ(fd, -1);
  EXPECT_EQ(errno, ENOENT);
}

TEST_F(UinputTest, UiDevCreateDestroyKeyboardEvIoGid) {
  auto ls_before = lsDir("/dev/input");
  uinput_setup usetup{.id = {.bustype = BUS_USB, .vendor = GOOGLE_VENDOR_ID, .product = 5}};
  strcpy(usetup.name, "Example device");
  int res = ioctl(uinput_fd_.get(), UI_DEV_SETUP, &usetup);
  ASSERT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_DEV_CREATE);
  ASSERT_EQ(res, 0);

  auto ls_after = lsDir("/dev/input");
  auto diff = lsDiff(ls_after, ls_before);
  ASSERT_EQ(diff.size(), 1u);

  auto new_device_name = diff[0];
  EXPECT_EQ(new_device_name.substr(0, std::string("event").length()), "event");

  {
    auto new_device_fd =
        test_helper::ScopedFD(open(("/dev/input/" + new_device_name).c_str(), O_RDWR));
    ASSERT_TRUE(new_device_fd.is_valid());
    input_id got_input_id;
    res = ioctl(new_device_fd.get(), EVIOCGID, &got_input_id);
    EXPECT_EQ(res, 0);
    EXPECT_EQ(got_input_id.bustype, BUS_USB);
    EXPECT_EQ(got_input_id.vendor, GOOGLE_VENDOR_ID);
    EXPECT_EQ(got_input_id.product, usetup.id.product);
  }

  res = ioctl(uinput_fd_.get(), UI_DEV_DESTROY);
  ASSERT_EQ(res, 0);

  // Check that the device file no longer exits. Open should fail.
  int fd = open(("/dev/input/" + new_device_name).c_str(), O_RDWR);
  EXPECT_EQ(fd, -1);
  EXPECT_EQ(errno, ENOENT);
}

TEST_F(UinputTest, UiDevDestroy) {
  uinput_setup usetup{.id = {.bustype = BUS_USB, .vendor = GOOGLE_VENDOR_ID, .product = 5}};
  strcpy(usetup.name, "Example device");
  int res = ioctl(uinput_fd_.get(), UI_DEV_SETUP, &usetup);
  ASSERT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_DEV_CREATE);
  ASSERT_EQ(res, 0);

  res = ioctl(uinput_fd_.get(), UI_DEV_DESTROY);
  EXPECT_EQ(res, 0);
}

TEST_F(UinputTest, WriteEVKEY) {
  // Need to create Keyboard device first
  uinput_setup usetup{.id = {.bustype = BUS_USB, .vendor = GOOGLE_VENDOR_ID, .product = 2}};
  strcpy(usetup.name, "Example device");
  int r = ioctl(uinput_fd_.get(), UI_DEV_SETUP, &usetup);
  ASSERT_EQ(r, 0);

  r = ioctl(uinput_fd_.get(), UI_DEV_CREATE);
  EXPECT_EQ(r, 0);

  /* timestamp values are ignored */
  struct timeval t = {.tv_sec = 0, .tv_usec = 0};

  // Key press
  struct input_event press_e = {.time = t, .type = EV_KEY, .code = KEY_SPACE, .value = 1};
  auto res = write(uinput_fd_.get(), &press_e, sizeof(press_e));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(press_e)));

  // Report the event
  struct input_event sync_e = {.time = t, .type = EV_SYN, .code = SYN_REPORT, .value = 0};
  res = write(uinput_fd_.get(), &sync_e, sizeof(sync_e));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(sync_e)));

  // Key release
  struct input_event release_e = {.time = t, .type = EV_KEY, .code = KEY_SPACE, .value = 0};
  res = write(uinput_fd_.get(), &release_e, sizeof(release_e));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(release_e)));

  // Report the event
  res = write(uinput_fd_.get(), &sync_e, sizeof(sync_e));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(sync_e)));
}

TEST_F(UinputTest, WriteEVABS) {
  // Need to create Touchscreen device first
  int r = ioctl(uinput_fd_.get(), UI_SET_EVBIT, EV_ABS);
  ASSERT_EQ(r, 0);

  uinput_setup usetup{.id = {.bustype = BUS_USB, .vendor = GOOGLE_VENDOR_ID, .product = 2}};
  strcpy(usetup.name, "Example device");
  r = ioctl(uinput_fd_.get(), UI_DEV_SETUP, &usetup);
  ASSERT_EQ(r, 0);

  r = ioctl(uinput_fd_.get(), UI_DEV_CREATE);
  EXPECT_EQ(r, 0);

  /* timestamp values are ignored */
  struct timeval t = {.tv_sec = 0, .tv_usec = 0};

  // Touch contact
  struct input_event ev_slot = {.time = t, .type = EV_ABS, .code = ABS_MT_SLOT, .value = 0};
  struct input_event ev_tracking_id = {
      .time = t, .type = EV_ABS, .code = ABS_MT_TRACKING_ID, .value = 0};
  struct input_event ev_pos_x = {.time = t, .type = EV_ABS, .code = ABS_MT_POSITION_X, .value = 10};
  struct input_event ev_pos_y = {.time = t, .type = EV_ABS, .code = ABS_MT_POSITION_Y, .value = 10};
  auto res = write(uinput_fd_.get(), &ev_slot, sizeof(ev_slot));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(ev_slot)));
  res = write(uinput_fd_.get(), &ev_tracking_id, sizeof(ev_tracking_id));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(ev_tracking_id)));
  res = write(uinput_fd_.get(), &ev_pos_x, sizeof(ev_pos_x));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(ev_pos_x)));
  res = write(uinput_fd_.get(), &ev_pos_y, sizeof(ev_pos_y));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(ev_pos_y)));

  // Report the event
  struct input_event sync_e = {.time = t, .type = EV_SYN, .code = SYN_REPORT, .value = 0};
  res = write(uinput_fd_.get(), &sync_e, sizeof(sync_e));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(sync_e)));

  // Touch contact released
  struct input_event ev_tracking_id_lifted = {
      .time = t, .type = EV_ABS, .code = ABS_MT_TRACKING_ID, .value = -1};
  res = write(uinput_fd_.get(), &ev_slot, sizeof(ev_slot));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(ev_slot)));
  res = write(uinput_fd_.get(), &ev_tracking_id_lifted, sizeof(ev_tracking_id_lifted));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(ev_tracking_id_lifted)));

  // Report the event
  res = write(uinput_fd_.get(), &sync_e, sizeof(sync_e));
  EXPECT_EQ(res, static_cast<ssize_t>(sizeof(sync_e)));
}

}  // namespace
