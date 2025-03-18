// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "arch/sys_arch.h"

#include <trace.h>

#include <fbl/canary.h>
#include <kernel/mutex.h>
#include <kernel/semaphore.h>
#include <kernel/spinlock.h>
#include <kernel/thread.h>
#include <lk/init.h>
#include <lwip/sys.h>
#include <lwip/tcpip.h>

#define LOCAL_TRACE 2

struct sys_sem {
  Semaphore sem;
};

struct sys_mutex {
  Mutex mutex;
};

#define SYS_MBOX_SIZE 16

struct sys_mbox {
  fbl::Canary<fbl::magic("MBOX")> magic;

  Semaphore empty;
  Semaphore full;
  Mutex lock;

  int head;
  int tail;
  int size;
  void *queue[SYS_MBOX_SIZE];
};

/*-----------------------------------------------------------------------------------*/
/* Time */
u32_t sys_now(void) { return static_cast<u32_t>(current_mono_time()); }

/*-----------------------------------------------------------------------------------*/
/* Init */
void sys_init(void) {}

/*-----------------------------------------------------------------------------------*/
/* Threads */

struct thread_wrapper_data {
  lwip_thread_fn function;
  void *arg;
};

static int thread_wrapper(void *arg) {
  struct thread_wrapper_data *thread_data = (struct thread_wrapper_data *)arg;

  thread_data->function(thread_data->arg);

  /* we should never get here */
  delete thread_data;
  return 0;
}

sys_thread_t sys_thread_new(const char *name, lwip_thread_fn func, void *arg, int stacksize,
                            int prio) {
  LWIP_UNUSED_ARG(stacksize);
  LWIP_UNUSED_ARG(prio);

  fbl::AllocChecker ac;
  struct thread_wrapper_data *thread_data = new (&ac) struct thread_wrapper_data();
  if (!ac.check()) {
    return nullptr;
  }
  thread_data->function = func;
  thread_data->arg = arg;

  Thread *thread = reinterpret_cast<sys_thread_t>(
      Thread::Create(name, thread_wrapper, thread_data, DEFAULT_PRIORITY));
  thread->Detach();
  thread->Resume();
  return thread;
}

/*-----------------------------------------------------------------------------------*/
/* Mutex */

err_t sys_mutex_new(struct sys_mutex **mutex) {
  struct sys_mutex *mtx;

  fbl::AllocChecker ac;
  mtx = new (&ac) sys_mutex();
  if (!ac.check()) {
    return ERR_MEM;
  }
  *mutex = mtx;
  return ERR_OK;
}

void sys_mutex_lock(struct sys_mutex **mutex) TA_ACQ((*mutex)->mutex) { (*mutex)->mutex.Acquire(); }
void sys_mutex_unlock(struct sys_mutex **mutex) TA_REL((*mutex)->mutex) {
  (*mutex)->mutex.Release();
}

void sys_mutex_free(struct sys_mutex **mutex) { delete *mutex; }

/*-----------------------------------------------------------------------------------*/
/* Critical section */

#if SYS_LIGHTWEIGHT_PROT

namespace {
SpinLock gArchLock;
}

sys_prot_t sys_arch_protect(void) TA_ACQ(gArchLock) {
  interrupt_saved_state_t irqstate;
  gArchLock.AcquireIrqSave(irqstate);
  return irqstate;
}

void sys_arch_unprotect(sys_prot_t pval) TA_REL(gArchLock) { gArchLock.ReleaseIrqRestore(pval); }

#endif /* SYS_LIGHTWEIGHT_PROT */

/*-----------------------------------------------------------------------------------*/
/* Mailbox */

err_t sys_mbox_new(struct sys_mbox **mb, int size) {
  struct sys_mbox *mbox;
  LWIP_UNUSED_ARG(size);

  fbl::AllocChecker ac;
  mbox = new (&ac) sys_mbox();
  if (!ac.check()) {
    return ERR_MEM;
  }

  mbox->head = 0;
  mbox->tail = 0;

  return ERR_OK;
}

void sys_mbox_free(struct sys_mbox **mb) { delete *mb; }

int sys_mbox_valid(struct sys_mbox **mb) {
  return (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid();
}

void sys_mbox_post(struct sys_mbox **mb, void *msg) {
  struct sys_mbox *mbox;
  LWIP_ASSERT("invalid mbox", (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid());
  mbox = *mb;

  mbox->empty.Wait(Deadline::infinite());
  mbox->lock.Acquire();

  mbox->queue[mbox->head] = msg;
  mbox->head = (mbox->head + 1) % SYS_MBOX_SIZE;

  mbox->lock.Release();
  mbox->full.Post();
  LTRACE_EXIT;
}

err_t sys_mbox_trypost(struct sys_mbox **mb, void *msg) {
  struct sys_mbox *mbox;
  LWIP_ASSERT("invalid mbox", (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid());
  mbox = *mb;

  if (unlikely(mbox->empty.count() <= 0)) {
    return ERR_TIMEOUT;
  }

  mbox->lock.Acquire();

  mbox->queue[mbox->head] = msg;
  mbox->head = (mbox->head + 1) % SYS_MBOX_SIZE;

  mbox->lock.Release();
  mbox->full.Post();

  return ERR_OK;
}

u32_t sys_arch_mbox_tryfetch(struct sys_mbox **mb, void **msg) {
  struct sys_mbox *mbox;
  LWIP_ASSERT("invalid mbox", (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid());
  mbox = *mb;

  if (unlikely(mbox->full.count() <= 0)) {
    return SYS_MBOX_EMPTY;
  }

  mbox->lock.Acquire();

  *msg = mbox->queue[mbox->tail];
  mbox->tail = (mbox->tail + 1) % SYS_MBOX_SIZE;

  mbox->lock.Release();
  mbox->empty.Post();

  return 0;
}

u32_t sys_arch_mbox_fetch(struct sys_mbox **mb, void **msg, u32_t timeout) {
  struct sys_mbox *mbox;
  LWIP_ASSERT("invalid mbox", (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid());
  mbox = *mb;

  zx_status_t res;
  zx_instant_mono_t start = current_mono_time();

  res = mbox->full.Wait(timeout ? Deadline::after_mono(ZX_MSEC(timeout)) : Deadline::infinite());
  if (res == ZX_ERR_TIMED_OUT) {
    return SYS_ARCH_TIMEOUT;  // timeout ? SYS_ARCH_TIMEOUT : 0;
  }

  mbox->lock.Acquire();

  *msg = mbox->queue[mbox->tail];
  mbox->tail = (mbox->tail + 1) % SYS_MBOX_SIZE;

  mbox->lock.Release();
  mbox->empty.Post();

  return static_cast<u32_t>(current_mono_time() - start);
}

/*-----------------------------------------------------------------------------------*/
/* Semaphore */

static void tcpip_init_done(void *arg) { dprintf(INFO, "TPC/IP init done\n"); }

/* run lwip init as soon as threads are running */
void lwip_init_hook(uint level) { tcpip_init(&tcpip_init_done, nullptr); }

/* completely un-threadsafe implementation of errno */
/* TODO: pull from kernel TLS or some other thread local storage */
static int _errno;

int *__geterrno(void) { return &_errno; }

LK_INIT_HOOK(lwip, &lwip_init_hook, LK_INIT_LEVEL_USER - 1)
