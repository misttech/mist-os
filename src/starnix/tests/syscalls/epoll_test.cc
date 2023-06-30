// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <signal.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <thread>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/test_helper.h"

namespace {

// Our Linux sysroot doesn't seem to have tgkill() and gettid().
void DoTgkill(int tgid, int tid, int sig) { syscall(SYS_tgkill, tgid, tid, sig); }
pid_t DoGetTid() { return static_cast<pid_t>(syscall(SYS_gettid)); }

void NoOpSigHandler(int) {}

int EpollAdd(int epfd, int to_watch, uint32_t events, uint64_t data) {
  struct epoll_event event;
  event.events = events;
  event.data.u64 = data;
  return epoll_ctl(epfd, EPOLL_CTL_ADD, to_watch, &event);
}

// Implements the backend thread that responds to 1-byte socket messages for the LotsaSignals test.
// Quits when one of the sockets receives a 'q'.
void LotsaSignals_DoPong(int main_tid, int* socks) {
  test_helper::ScopedFD epfd(epoll_create(2));
  ASSERT_TRUE(epfd.is_valid());

  ASSERT_EQ(0, EpollAdd(epfd.get(), socks[0], EPOLLIN, 0));
  ASSERT_EQ(0, EpollAdd(epfd.get(), socks[1], EPOLLIN, 1));

  constexpr int kMaxEvents = 4;
  struct epoll_event out_events[kMaxEvents];
  while (true) {
    errno = 0;
    int result = HANDLE_EINTR(epoll_wait(epfd.get(), out_events, kMaxEvents, -1));
    ASSERT_GT(result, 0);

    // Send reply(s).
    for (int i = 0; i < result; i++) {
      int sock_id = static_cast<int>(out_events[i].data.u64);
      ASSERT_TRUE(sock_id == 0 || sock_id == 1);

      char in = 0;
      ASSERT_EQ(1, HANDLE_EINTR(read(socks[sock_id], &in, 1)));
      if (in == 'q')
        return;

      // Spam user signals both before and after the reply to try to trigger races.
      DoTgkill(main_tid, main_tid, SIGUSR1);
      ASSERT_EQ(1, HANDLE_EINTR(write(socks[sock_id], "a", 1)));
      DoTgkill(main_tid, main_tid, SIGUSR1);
    }
  }
}

class ScopedSignalHandler {
 public:
  ScopedSignalHandler(int signum, sighandler_t handler) : signum_(signum) {
    prev_handler_ = signal(signum, handler);
  }
  ~ScopedSignalHandler() { signal(signum_, prev_handler_); }

 private:
  int signum_;
  sighandler_t prev_handler_;
};

}  // namespace

// Ping-pongs a bunch of messages between two threads while spamming the main thread with signals.
// This tests that epoll doesn't issue any spurious wakes.
TEST(EpollTest, LotsaSignals) {
  ScopedSignalHandler handler(SIGUSR1, &NoOpSigHandler);

  int pair1[2];
  int result = socketpair(AF_UNIX, SOCK_STREAM, 0, pair1);
  ASSERT_EQ(result, 0);

  int pair2[2];
  result = socketpair(AF_UNIX, SOCK_STREAM, 0, pair2);
  ASSERT_EQ(result, 0);

  // Ping pong
  pid_t main_tid = DoGetTid();
  std::thread ponger(
      [main_tid, one = test_helper::ScopedFD(pair1[1]), two = test_helper::ScopedFD(pair2[1])]() {
        int socks[2];
        socks[0] = one.get();
        socks[1] = two.get();
        LotsaSignals_DoPong(main_tid, socks);
      });

  test_helper::ScopedFD epfd(epoll_create(2));
  ASSERT_TRUE(epfd.is_valid());

  test_helper::ScopedFD socks[2] = {test_helper::ScopedFD(pair1[0]),
                                    test_helper::ScopedFD(pair2[0])};
  ASSERT_EQ(0, EpollAdd(epfd.get(), socks[0].get(), EPOLLIN, 0));
  ASSERT_EQ(0, EpollAdd(epfd.get(), socks[1].get(), EPOLLIN, 1));

  HANDLE_EINTR(write(socks[0].get(), "0", 1));
  HANDLE_EINTR(write(socks[1].get(), "0", 1));

  // Arbitrary but large number of messages to send.
  constexpr int kMessageCount = 10000;

  constexpr int kMaxEvents = 4;
  struct epoll_event out_events[kMaxEvents];
  for (int i = 0; i < kMessageCount; i++) {
    errno = 0;
    int result = HANDLE_EINTR(epoll_wait(epfd.get(), out_events, kMaxEvents, -1));
    ASSERT_GT(result, 0);

    for (int i = 0; i < result; i++) {
      int sock_id = static_cast<int>(out_events[i].data.u64);
      ASSERT_TRUE(sock_id == 0 || sock_id == 1);

      // Read the message and reply to it.
      char in = 0;
      ASSERT_EQ(1, HANDLE_EINTR(read(socks[sock_id].get(), &in, 1)));
      ASSERT_EQ(1, HANDLE_EINTR(write(socks[sock_id].get(), "a", 1)));
    }
  }

  // Send quit to make the pong thread quit.
  ASSERT_EQ(1, HANDLE_EINTR(write(socks[0].get(), "q", 1)));

  ponger.join();
}

TEST(EpollTest, CloseAfterAdd) {
  int sockets[2];
  int result = socketpair(AF_UNIX, SOCK_STREAM, 0, sockets);
  ASSERT_EQ(0, result);

  int epfd = epoll_create(2);
  ASSERT_GE(epfd, 0);

  // Wait on socket[1] readable.
  struct epoll_event event;
  event.events = EPOLLIN;
  event.data.u64 = 1;
  result = epoll_ctl(epfd, EPOLL_CTL_ADD, sockets[1], &event);
  ASSERT_EQ(result, 0);

  // Write data in the "0" end so the "1" will be marked ready to read. Writing just one byte
  // ensures there can't be a short write.
  ASSERT_EQ(1, HANDLE_EINTR(write(sockets[0], "a", 1)));

  // Close the read socket out from under epoll.
  close(sockets[1]);

  // Waiting on the (now empty) epoll object should timeout rather than report it's ready to read
  // or that there's a bad file descriptor.
  result = epoll_wait(epfd, &event, 1, 1);
  EXPECT_EQ(0, result) << errno;
}

TEST(EpollTest, InvalidCreateSize) {
  errno = 0;
  EXPECT_EQ(-1, epoll_create(0));
  EXPECT_EQ(EINVAL, errno);

  errno = 0;
  EXPECT_EQ(-1, epoll_create(-1));
  EXPECT_EQ(EINVAL, errno);
}

TEST(EpollTest, WaitInvalidParams) {
  test_helper::ScopedFD epfd(epoll_create(2));
  ASSERT_TRUE(epfd);

  struct epoll_event event;

  errno = 0;
  EXPECT_EQ(-1, epoll_wait(epfd.get(), &event, 0, 0));
  EXPECT_EQ(EINVAL, errno);

  errno = 0;
  EXPECT_EQ(-1, epoll_wait(epfd.get(), &event, -1, 0));
  EXPECT_EQ(EINVAL, errno);

  // Pass invalid event pointer but valid count.  Linux seems to believe that
  // valid means 0 <= ptr < process memory - size of maxevents.
  errno = 0;
  EXPECT_EQ(
      -1, epoll_wait(epfd.get(), reinterpret_cast<struct epoll_event*>(0xFFFFFFFFFFFFFFFF), 1, 0));
  EXPECT_EQ(EFAULT, errno);

  // Linux believes nullptr is okay, so testing that
  EXPECT_EQ(0, epoll_wait(epfd.get(), nullptr, 1, 0));

  // When both the pointer and the count are invalid, Linux returns EINVAL (it checks the count
  // first).
  errno = 0;
  EXPECT_EQ(-1, epoll_wait(epfd.get(), nullptr, 0, 0));
  EXPECT_EQ(EINVAL, errno);
}
