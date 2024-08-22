// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/console.h>
#include <lib/ktrace.h>
#include <lib/unittest/user_memory.h>

static int cmd_ktrace(int argc, const cmd_args *argv, uint32_t flags);

STATIC_COMMAND_START
STATIC_COMMAND_MASKED("ktrace", "control ktrace from kernel", &cmd_ktrace, CMD_AVAIL_NORMAL)
STATIC_COMMAND_END(ktrace)

constexpr char kUsage[] =
    "\
Usage: ktrace [options] <control>\n\
Where <control> is one of:\n\
  start <group_mask>  - start tracing\n\
  stop                - stop tracing\n\
  rewind              - rewind trace buffer\n\
  written             - print bytes written to trace buffer\n\
    Note: This value doesn't reset on \"rewind\". Instead, the rewind\n\
    takes effect on the next \"start\".\n\
  dump          - dump contents of trace buffer to console\n\
\n\
Options:\n\
  --help  - Duh.\n\
";

static int hexval(char c) {
  if (c >= '0' && c <= '9')
    return c - '0';
  else if (c >= 'a' && c <= 'f')
    return c - 'a' + 10;
  else if (c >= 'A' && c <= 'F')
    return c - 'A' + 10;

  return 0;
}

int isdigit(int c) { return ((c >= '0') && (c <= '9')); }

int isxdigit(int c) {
  return isdigit(c) || ((c >= 'a') && (c <= 'f')) || ((c >= 'A') && (c <= 'F'));
}

long atol(const char *num) {
  long value = 0;
  int neg = 0;

  if (num[0] == '0' && num[1] == 'x') {
    // hex
    num += 2;
    while (*num && isxdigit(*num))
      value = value * 16 + hexval(*num++);
  } else {
    // decimal
    if (num[0] == '-') {
      neg = 1;
      num++;
    }
    while (*num && isdigit(*num))
      value = value * 10 + *num++ - '0';
  }

  if (neg)
    value = -value;

  return value;
}

int atoi(const char *num) { return static_cast<int>(atol(num)); }

int dump(void *arg) {
  char buf[PAGE_SIZE];
  int count = 0;
  uint32_t offset = 0;
  ssize_t bytes_written;
  auto user = testing::UserMemory::Create(PAGE_SIZE);
  user->CommitAndMap(PAGE_SIZE);

  printf("Dump FXT:\n");
  while ((bytes_written = ktrace_read_user(user->user_out<void>(), offset, PAGE_SIZE)) > 0) {
    user->VmoRead(buf, 0, bytes_written);
    offset += bytes_written;

    for (int i = 0; i < bytes_written; i++) {
      printf("%02hhx ", *(uint8_t *)(buf + i));
      if ((++count % 16) == 0) {
        printf("\n");
      }
    }
  }

  if (bytes_written < 0) {
    printf("Error getting bytes written: (%d)\n", static_cast<zx_status_t>(bytes_written));
    return -1;
  }
  return 0;
}

bool dump_userpace() {
  fbl::RefPtr<VmAspace> aspace = VmAspace::Create(VmAspace::Type::User, "ktrace_dump");
  if (!aspace) {
    printf("failed to create user aspace\n");
    return false;
  }
  auto destroy_aspace = fit::defer([&]() {
    zx_status_t status = aspace->Destroy();
    DEBUG_ASSERT(status == ZX_OK);
  });
  Thread *t = Thread::Create("ktrace_dump", dump, nullptr, DEFAULT_PRIORITY);
  if (!t) {
    printf("failed to create thread\n");
    return false;
  }
  aspace->AttachToThread(t);

  t->Resume();
  int success = 0;
  zx_status_t status = t->Join(&success, ZX_TIME_INFINITE);
  if (status != ZX_OK) {
    printf("failed to join thread: %d\n", status);
    return false;
  }
  return success;
}

static int cmd_ktrace(int argc, const cmd_args *argv, uint32_t flags) {
  if (argc >= 2 && strcmp(argv[1].str, "--help") == 0) {
    printf(kUsage);
    return 0;
  }

  if (argc < 2) {
    printf(kUsage);
    return -1;
  }

  if (strcmp(argv[1].str, "start") == 0) {
    int group_mask = atoi(argv[2].str);
    if (group_mask < 0) {
      printf("Invalid group mask\n");
      return -1;
    }
    return KTRACE_STATE.Start(group_mask, internal::KTraceState::StartMode::Saturate);
  } else if (strcmp(argv[1].str, "stop") == 0) {
    return KTRACE_STATE.Stop();
  } else if (strcmp(argv[1].str, "rewind") == 0) {
    return KTRACE_STATE.Rewind();
  } else if (strcmp(argv[1].str, "written") == 0) {
    ssize_t bytes_written = ktrace_read_user(user_out_ptr<void>(nullptr), 0, 0);
    if (bytes_written < 0) {
      printf("Error getting bytes written: (%d)\n", static_cast<zx_status_t>(bytes_written));
      return -1;
    }
    printf("Bytes written: %ld\n", bytes_written);
  } else if (strcmp(argv[1].str, "dump") == 0) {
    return dump_userpace() ? 0 : -1;
  }
  return 0;
}
