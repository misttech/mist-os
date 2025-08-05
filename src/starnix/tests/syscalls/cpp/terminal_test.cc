// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sys/ioctl.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/sysmacros.h>
#include <termios.h>

#include <string>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

int g_received_signal[64] = {};

void RecordSignalHandler(int signo) { g_received_signal[signo]++; }

void IgnoreSignal(int signal) {
  struct sigaction action;
  action.sa_handler = SIG_IGN;
  SAFE_SYSCALL(sigaction(signal, &action, nullptr));
}

void RecordSignal(int signal) {
  g_received_signal[signal] = 0;
  struct sigaction action;
  action.sa_handler = RecordSignalHandler;
  SAFE_SYSCALL(sigaction(signal, &action, nullptr));
}

long SleepNs(uint64_t count) {
  const uint64_t NS_PER_SECONDS = 1000000000;
  struct timespec ts = {.tv_sec = static_cast<time_t>(count / NS_PER_SECONDS),
                        .tv_nsec = static_cast<long>(count % NS_PER_SECONDS)};
  // TODO(qsr): Use nanosleep when starnix implements clock_nanosleep
  return syscall(SYS_nanosleep, &ts, nullptr);
}

int OpenMainTerminal(int additional_flags = 0) {
  int fd = SAFE_SYSCALL(posix_openpt(O_RDWR | additional_flags));
  SAFE_SYSCALL(grantpt(fd));
  SAFE_SYSCALL(unlockpt(fd));
  return fd;
}

TEST(JobControl, BackgroundProcessGroupDoNotUpdateOnDeath) {
  // Assume starnix always has /dev/ptmx mapped in.
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::IsStarnix() && access("/dev/ptmx", F_OK) == -1) {
    GTEST_SKIP() << "Pseudoterminal not available, skipping...";
  }
  test_helper::ForkHelper helper;

  IgnoreSignal(SIGTTOU);

  helper.RunInForkedProcess([&] {
    SAFE_SYSCALL(setsid());
    int main_terminal = OpenMainTerminal();
    int replica_terminal = SAFE_SYSCALL(open(ptsname(main_terminal), O_RDWR));

    ASSERT_EQ(SAFE_SYSCALL(tcgetpgrp(replica_terminal)), getpid());
    pid_t child_pid = helper.RunInForkedProcess([&] {
      SAFE_SYSCALL(setpgid(0, 0));
      SAFE_SYSCALL(tcsetpgrp(replica_terminal, getpid()));

      ASSERT_EQ(SAFE_SYSCALL(tcgetpgrp(replica_terminal)), getpid());
    });

    // Wait for the child to die.
    ASSERT_TRUE(helper.WaitForChildren());

    // The foreground process group should still be the one from the child.
    ASSERT_EQ(SAFE_SYSCALL(tcgetpgrp(replica_terminal)), child_pid);

    ASSERT_EQ(setpgid(0, child_pid), -1)
        << "Expected not being able to join a process group that has no member anymore";
    ASSERT_EQ(errno, EPERM);
  });
}

TEST(JobControl, OrphanedProcessGroupsReceivesSignal) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([&] {
    // Create a new session here, and associate it with the new terminal.
    SAFE_SYSCALL(setsid());

    helper.RunInForkedProcess([&] {
      // Create a new, non leader, process group.
      SAFE_SYSCALL(setpgid(0, 0));
      pid_t pid = helper.RunInForkedProcess([&] {
        // Deepest child. Set a SIGHUP handler, stop ourself, and check that we
        // are restarted and received the expected SIGHUP when our immediate
        // parent dies
        RecordSignal(SIGHUP);
        SAFE_SYSCALL(kill(getpid(), SIGTSTP));
        // At this point, a SIGHUP should have been received.
        // TODO(qsr): Remove the syscall that is there only because starnix
        // currently doesn't handle signal outside of syscalls, and doesn't
        // handle multiple signals at once.
        SAFE_SYSCALL(getpid());
        EXPECT_EQ(g_received_signal[SIGHUP], 1);
      });
      // Wait for the child to have stopped.
      SAFE_SYSCALL(waitid(P_PID, pid, nullptr, WSTOPPED));
    });
    // Wait for the child to die.
    ASSERT_TRUE(helper.WaitForChildren());
  });
}

class Pty : public testing::Test {
  void SetUp() {
    // Assume starnix always has /dev/ptmx mapped in.
    // TODO(https://fxbug.dev/317285180) don't skip on baseline
    if (!test_helper::IsStarnix() && access("/dev/ptmx", F_OK) == -1) {
      GTEST_SKIP() << "Pseudoterminal not available, skipping...";
    }
  }
};

TEST_F(Pty, SIGWINCH) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([&] {
    // Create a new session here, and associate it with the new terminal.
    SAFE_SYSCALL(setsid());
    int main_terminal = OpenMainTerminal();
    SAFE_SYSCALL(ioctl(main_terminal, TIOCSCTTY, 0));

    // Register a signal handler for sigusr1.
    RecordSignal(SIGUSR1);
    IgnoreSignal(SIGTTOU);
    IgnoreSignal(SIGHUP);

    // fork a child, move it to its own process group and makes it the
    // foreground one.
    helper.RunInForkedProcess([&] {
      SAFE_SYSCALL(setpgid(0, 0));
      SAFE_SYSCALL(tcsetpgrp(main_terminal, getpid()));

      // Register a signal handler for sigwinch.
      IgnoreSignal(SIGUSR1);
      RecordSignal(SIGWINCH);

      // Send a SIGUSR1 to notify our parent.
      SAFE_SYSCALL(kill(getppid(), SIGUSR1));

      // Wait for a SIGWINCH
      while (g_received_signal[SIGWINCH] == 0) {
        SleepNs(10e7);
      }
    });
    // Wait for SIGUSR1
    while (g_received_signal[SIGUSR1] == 0) {
      SleepNs(10e7);
    }

    // Resize the window, which must generate a SIGWINCH for the children.
    struct winsize ws = {.ws_row = 10, .ws_col = 10};
    SAFE_SYSCALL(ioctl(main_terminal, TIOCSWINSZ, &ws));
  });
}

ssize_t FullRead(int fd, char* buf, size_t count) {
  ssize_t result = 0;
  while (count > 0) {
    ssize_t read_result = read(fd, buf, count);
    if (read_result == -1) {
      if (errno == EAGAIN) {
        break;
      }
      return -1;
    }
    buf += read_result;
    count -= read_result;
    result += read_result;
  }
  return result;
}

TEST_F(Pty, OpenDevTTY) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([&] {
    // Create a new session here, and associate it with the new terminal.
    SAFE_SYSCALL(setsid());

    int main_terminal = OpenMainTerminal(O_NONBLOCK);
    SAFE_SYSCALL(ioctl(main_terminal, TIOCSCTTY, 0));

    SAFE_SYSCALL(open("/dev/tty", O_RDWR));
    int other_terminal = SAFE_SYSCALL(open("/dev/tty", O_RDWR));
    struct stat stats;
    SAFE_SYSCALL(fstat(other_terminal, &stats));

    ASSERT_EQ(major(stats.st_rdev), 5u);
    ASSERT_EQ(minor(stats.st_rdev), 0u);

    ASSERT_EQ(write(other_terminal, "h\n", 2), 2);
    char buf[20];
    ASSERT_EQ(FullRead(main_terminal, buf, 20), 3);
    ASSERT_EQ(strncmp(buf, "h\r\n", 3), 0);
  });
}

TEST_F(Pty, ioctl_TCSETSF) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([&] {
    // Create a new session here, and associate it with the new terminal.
    SAFE_SYSCALL(setsid());
    int main_terminal = OpenMainTerminal();

    struct termios config;
    SAFE_SYSCALL(ioctl(main_terminal, TCGETS, &config));
    SAFE_SYSCALL(ioctl(main_terminal, TCSETSF, &config));
  });
}

void FullWrite(int fd, const char* buffer, ssize_t size) {
  ASSERT_EQ(write(fd, buffer, size), size);
}

TEST_F(Pty, EndOfFile) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([&] {
    // Create a new session here.
    SAFE_SYSCALL(setsid());
    int main_terminal = OpenMainTerminal();
    int replica_terminal = SAFE_SYSCALL(open(ptsname(main_terminal), O_RDWR | O_NONBLOCK));

    char source_buffer[2];
    source_buffer[0] = 4;  // ^D
    source_buffer[1] = '\n';
    char target_buffer[2];

    FullWrite(main_terminal, source_buffer, 1);
    ASSERT_EQ(0, SAFE_SYSCALL(read(replica_terminal, target_buffer, 2)));
    ASSERT_EQ(-1, read(replica_terminal, target_buffer, 2));
    ASSERT_EQ(EAGAIN, errno);

    FullWrite(main_terminal, source_buffer, 2);
    ASSERT_EQ(0, SAFE_SYSCALL(read(replica_terminal, target_buffer, 2)));
    ASSERT_EQ(1, SAFE_SYSCALL(read(replica_terminal, target_buffer, 2)));
    ASSERT_EQ('\n', target_buffer[0]);

    FullWrite(main_terminal, source_buffer, 1);
    FullWrite(main_terminal, source_buffer + 1, 1);
    ASSERT_EQ(0, SAFE_SYSCALL(read(replica_terminal, target_buffer, 2)));
    ASSERT_EQ(1, SAFE_SYSCALL(read(replica_terminal, target_buffer, 2)));
    ASSERT_EQ('\n', target_buffer[0]);

    source_buffer[0] = 4;  // ^D
    source_buffer[1] = 4;  // ^D
    FullWrite(main_terminal, source_buffer, 2);
    ASSERT_EQ(0, SAFE_SYSCALL(read(replica_terminal, target_buffer, 2)));
    ASSERT_EQ(0, SAFE_SYSCALL(read(replica_terminal, target_buffer, 2)));
    ASSERT_EQ(-1, read(replica_terminal, target_buffer, 2));
    ASSERT_EQ(EAGAIN, errno);

    source_buffer[0] = ' ';
    source_buffer[1] = 4;  // ^D
    FullWrite(main_terminal, source_buffer, 2);
    ASSERT_EQ(1, SAFE_SYSCALL(read(replica_terminal, target_buffer, 2)));
    ASSERT_EQ(' ', target_buffer[0]);
  });
}

TEST_F(Pty, EchoModes) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([&] {
    // Create a new session here.
    SAFE_SYSCALL(setsid());
    int main_terminal = OpenMainTerminal();
    int replica_terminal = SAFE_SYSCALL(open(ptsname(main_terminal), O_RDWR | O_NONBLOCK));

    unsigned default_lflags = ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN;

    struct termios termios = {};
    ASSERT_EQ(0, SAFE_SYSCALL(tcgetattr(main_terminal, &termios)));
    ASSERT_EQ(default_lflags, termios.c_lflag);
    memset(&termios, 0, sizeof(termios));
    ASSERT_EQ(0, SAFE_SYSCALL(tcgetattr(replica_terminal, &termios)));
    ASSERT_EQ(default_lflags, termios.c_lflag);

    auto check_input = [main_terminal, replica_terminal](const char* input, const char* main_str,
                                                         const char* replicate_str) {
      char target_buffer[64] = {};

      FullWrite(main_terminal, input, strlen(input));
      ASSERT_EQ((ssize_t)strlen(main_str),
                SAFE_SYSCALL(read(main_terminal, target_buffer, sizeof(target_buffer) - 1)));
      ASSERT_STREQ(main_str, target_buffer);
      memset(target_buffer, 0, sizeof(target_buffer));
      ASSERT_EQ((ssize_t)strlen(replicate_str),
                SAFE_SYSCALL(read(replica_terminal, target_buffer, sizeof(target_buffer) - 1)));
      ASSERT_STREQ(replicate_str, target_buffer);
    };

    // clang-format off
    check_input("ab\x7F" "cd\n", "ab\b \bcd\r\n", "acd\n");
    check_input("ab\x01" "cd\n", "ab^Acd\r\n", "ab\x01" "cd\n");
    check_input("ab\x06" "cd\n", "ab^Fcd\r\n", "ab\x06" "cd\n");
    check_input("ab\x07" "cd\n", "ab^Gcd\r\n", "ab\x07" "cd\n");
    check_input("ab\x08" "cd\n", "ab^Hcd\r\n", "ab\x08" "cd\n");
    check_input("ab\x09" "cd\n", "ab\tcd\r\n", "ab\tcd\n");
    check_input("ab\x0E" "cd\n", "ab^Ncd\r\n", "ab\x0E" "cd\n");
    check_input("ab\x0F" "cd\n", "ab^Ocd\r\n", "ab\x0F" "cd\n");
    check_input("ab\x15" "cd\n", "ab\b \b\b \bcd\r\n", "cd\n");
    check_input("ab\x1B" "cd\n", "ab^[cd\r\n", "ab\x1B" "cd\n");
    // clang-format on
  });
}
TEST_F(Pty, EchoFlags) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([&] {
    // Create a new session here.
    SAFE_SYSCALL(setsid());
    int main_terminal = OpenMainTerminal();
    int replica_terminal = SAFE_SYSCALL(open(ptsname(main_terminal), O_RDWR | O_NONBLOCK));

    struct termios termios = {};
    ASSERT_EQ(0, SAFE_SYSCALL(tcgetattr(main_terminal, &termios)));

    auto check_input = [main_terminal, replica_terminal](unsigned int lflags, const char* input,
                                                         const char* main_str,
                                                         const char* replicate_str) {
      char target_buffer[64] = {};

      // Set the flags
      struct termios termios = {};
      ASSERT_EQ(0, SAFE_SYSCALL(tcgetattr(main_terminal, &termios)));
      termios.c_lflag = lflags;
      ASSERT_EQ(0, SAFE_SYSCALL(tcsetattr(main_terminal, TCSANOW, &termios)));

      FullWrite(main_terminal, input, strlen(input));
      ASSERT_EQ((ssize_t)strlen(main_str),
                SAFE_SYSCALL(read(main_terminal, target_buffer, sizeof(target_buffer) - 1)));
      ASSERT_STREQ(main_str, target_buffer);
      memset(target_buffer, 0, sizeof(target_buffer));
      ASSERT_EQ((ssize_t)strlen(replicate_str),
                SAFE_SYSCALL(read(replica_terminal, target_buffer, sizeof(target_buffer) - 1)));
      ASSERT_STREQ(replicate_str, target_buffer);
    };

    // Test different combinations of echo flags
    unsigned base_lflags = ISIG | ICANON | ECHO | ECHOCTL | IEXTEN;

    // Just ECHO
    check_input(base_lflags, "abc\n", "abc\r\n", "abc\n");

    // ECHO + ECHOE (erase char)
    check_input(base_lflags | ECHOE,
                "ab\x7F"
                "c\n",
                "ab\b \bc\r\n", "ac\n");

    // ECHO + ECHOK (kill line)
    check_input(base_lflags | ECHOK,
                "ab\x15"
                "c\n",
                "ab^U\r\nc\r\n", "c\n");

    // ECHO + ECHOE + ECHOK (erase char + kill line)
    check_input(base_lflags | ECHOE | ECHOK,
                "ab\x7F\x15"
                "c\n",
                "ab\b \b^U\r\nc\r\n", "c\n");

    // ECHO + ECHOE + ECHOK + ECHOKE (erase char + kill line + kill line erase)
    check_input(base_lflags | ECHOE | ECHOK | ECHOKE,
                "abc\x7F\x15"
                "d\n",
                "abc\b \b\b \b\b \bd\r\n", "d\n");
  });
}

TEST_F(Pty, SendSignals) {
  test_helper::ForkHelper helper;

  std::map<int, char> signal_and_control_character;
  signal_and_control_character[SIGINT] = 3;
  signal_and_control_character[SIGQUIT] = 28;
  signal_and_control_character[SIGSTOP] = 26;

  for (auto [s, c] : signal_and_control_character) {
    auto signal = s;
    auto character = c;

    helper.RunInForkedProcess([&] {
      // Create a new session here, and associate it with the new terminal.
      SAFE_SYSCALL(setsid());
      int main_terminal = OpenMainTerminal();
      SAFE_SYSCALL(ioctl(main_terminal, TIOCSCTTY, 0));

      // Register a signal handler for sigusr1.
      RecordSignal(SIGUSR1);
      IgnoreSignal(SIGTTOU);
      IgnoreSignal(SIGHUP);

      // fork a child, move it to its own process group and makes it the
      // foreground one.
      pid_t child_pid = helper.RunInForkedProcess([&] {
        SAFE_SYSCALL(setpgid(0, 0));
        SAFE_SYSCALL(tcsetpgrp(main_terminal, getpid()));

        // Send a SIGUSR1 to notify our parent.
        SAFE_SYSCALL(kill(getppid(), SIGUSR1));

        // Wait to be killed by our parent.
        for (;;) {
          SleepNs(10e8);
        }
      });
      // Wait for SIGUSR1
      while (g_received_signal[SIGUSR1] == 0) {
        SleepNs(10e7);
      }

      // Send control character.
      char buffer[1];
      buffer[0] = character;
      SAFE_SYSCALL(write(main_terminal, buffer, 1));

      int wstatus;
      pid_t received_pid = SAFE_SYSCALL(waitpid(child_pid, &wstatus, WUNTRACED));
      ASSERT_EQ(received_pid, child_pid);
      if (signal == SIGSTOP) {
        ASSERT_TRUE(WIFSTOPPED(wstatus));
        // Ensure the children is called, even when only stopped.
        SAFE_SYSCALL(kill(child_pid, SIGKILL));
        SAFE_SYSCALL(waitpid(child_pid, NULL, 0));
      } else {
        ASSERT_TRUE(WIFSIGNALED(wstatus));
        ASSERT_EQ(WTERMSIG(wstatus), signal);
      }
    });
    ASSERT_TRUE(helper.WaitForChildren());
  }
}

TEST_F(Pty, CloseMainTerminal) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    IgnoreSignal(SIGHUP);
    // Create a new session here, and associate it with the new terminal.
    SAFE_SYSCALL(setsid());
    int main_terminal = OpenMainTerminal(O_NONBLOCK | O_NOCTTY);
    int replica_terminal =
        SAFE_SYSCALL(open(ptsname(main_terminal), O_RDWR | O_NONBLOCK | O_NOCTTY));
    ASSERT_EQ(open("/dev/tty", O_RDWR), -1);
    ASSERT_EQ(errno, ENXIO);
    close(main_terminal);
    char buffer[1];
    ASSERT_EQ(read(replica_terminal, buffer, 1), 0);
    ASSERT_EQ(write(replica_terminal, buffer, 1), -1);
    EXPECT_EQ(EIO, errno);

    short all_events = POLLIN | POLLPRI | POLLOUT | POLLRDHUP | POLLERR | POLLHUP | POLLNVAL;
    struct pollfd fds = {replica_terminal, all_events, 0};
    ASSERT_EQ(1, SAFE_SYSCALL(poll(&fds, 1, -1)));
    EXPECT_EQ(fds.revents, POLLIN | POLLOUT | POLLERR | POLLHUP);
  });
}

TEST_F(Pty, CloseReplicaTerminal) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    // Create a new session here, and associate it with the new terminal.
    SAFE_SYSCALL(setsid());
    int main_terminal = OpenMainTerminal(O_NONBLOCK | O_NOCTTY);
    int replica_terminal =
        SAFE_SYSCALL(open(ptsname(main_terminal), O_RDWR | O_NONBLOCK | O_NOCTTY));
    ASSERT_EQ(open("/dev/tty", O_RDWR), -1);
    ASSERT_EQ(errno, ENXIO);
    close(replica_terminal);

    char buffer[1];
    ASSERT_EQ(read(main_terminal, buffer, 1), -1);
    EXPECT_EQ(EIO, errno);

    short all_events = POLLIN | POLLPRI | POLLOUT | POLLRDHUP | POLLERR | POLLHUP | POLLNVAL;
    struct pollfd fds = {main_terminal, all_events, 0};
    ASSERT_EQ(1, SAFE_SYSCALL(poll(&fds, 1, -1)));
    ASSERT_EQ(fds.revents, POLLOUT | POLLHUP);

    ASSERT_EQ(write(main_terminal, buffer, 1), 1);
  });
}

TEST_F(Pty, DetectReplicaClosing) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    // Create a new session here, and associate it with the new terminal.
    SAFE_SYSCALL(setsid());
    int main_terminal = OpenMainTerminal(O_NOCTTY);
    int replica_terminal = SAFE_SYSCALL(open(ptsname(main_terminal), O_RDWR | O_NOCTTY));

    struct pollfd fds = {main_terminal, POLLIN, 0};

    RecordSignal(SIGUSR1);
    pid_t child_pid = helper.RunInForkedProcess([&] {
      close(main_terminal);
      RecordSignal(SIGUSR2);
      SAFE_SYSCALL(kill(getppid(), SIGUSR1));
      // Wait for SIGUSR2
      while (g_received_signal[SIGUSR2] == 0) {
        SleepNs(10e7);
      }
    });

    close(replica_terminal);
    // Wait for SIGUSR1
    while (g_received_signal[SIGUSR1] == 0) {
      SleepNs(10e7);
    }
    SAFE_SYSCALL(kill(child_pid, SIGUSR2));
    ASSERT_EQ(1, SAFE_SYSCALL(HANDLE_EINTR(poll(&fds, 1, 10000))));
    ASSERT_EQ(fds.revents, POLLHUP);
  });
}

TEST_F(Pty, NewInstance) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (getuid() != 0) {
    GTEST_SKIP() << "Can only be run as root.";
  }

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    test_helper::ScopedTempDir mount_point1, mount_point2;

    // Mount a default devpts instance.
    auto mount1 = ASSERT_RESULT_SUCCESS_AND_RETURN(
        test_helper::ScopedMount::Mount("devpts", mount_point1.path(), "devpts", 0, nullptr));

    // Mount a new devpts instance.
    auto mount2 = ASSERT_RESULT_SUCCESS_AND_RETURN(
        test_helper::ScopedMount::Mount("devpts", mount_point2.path(), "devpts", 0, "newinstance"));

    struct stat stat_buf;

    // Open ptmx in the first instance, which should create pts/0.
    std::string ptmx1_path = mount_point1.path() + "/ptmx";
    fbl::unique_fd ptmx1_fd(open(ptmx1_path.c_str(), O_RDWR));
    ASSERT_TRUE(ptmx1_fd.is_valid());
    std::string pts1_0_path = mount_point1.path() + "/0";
    ASSERT_EQ(0, stat(pts1_0_path.c_str(), &stat_buf));

    // The two instances should be separate. Opening a pty in the second instance
    // should not create a new pty in the first one.
    std::string pts2_0_path = mount_point2.path() + "/0";
    ASSERT_EQ(-1, stat(pts2_0_path.c_str(), &stat_buf));
    ASSERT_EQ(ENOENT, errno);

    // Open ptmx in the second instance, which should now create pts/0.
    std::string ptmx2_path = mount_point2.path() + "/ptmx";
    fbl::unique_fd ptmx2_fd(open(ptmx2_path.c_str(), O_RDWR));
    ASSERT_TRUE(ptmx2_fd.is_valid());
    ASSERT_EQ(0, stat(pts2_0_path.c_str(), &stat_buf));
  });
}

}  // namespace
