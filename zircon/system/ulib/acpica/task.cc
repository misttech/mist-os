// Copyright 2025 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/sync/condvar.h>

#include <acpica/acpi.h>
#include <fbl/intrusive_double_list.h>
#include <kernel/event.h>
#include <kernel/mutex.h>
#include <kernel/thread.h>
#include <ktl/unique_ptr.h>

/* Structures used for implementing AcpiOsExecute and
 * AcpiOsWaitEventsComplete */
struct AcpiOsTaskCtx : public fbl::DoublyLinkedListable<ktl::unique_ptr<AcpiOsTaskCtx>> {
  ACPI_OSD_EXEC_CALLBACK func;
  void* ctx;
};

/* Thread function for implementing AcpiOsExecute */
static int AcpiOsExecuteTask(void* arg);

/* Data used for implementing AcpiOsExecute and
 * AcpiOsWaitEventsComplete */
static struct os_execute_state_t {
  Thread* thread;
  sync::CondVar cond;
  sync::CondVar idle_cond;
  DECLARE_MUTEX(os_execute_state_t) lock;
  bool shutdown = false;
  bool idle = true;

  fbl::DoublyLinkedList<ktl::unique_ptr<AcpiOsTaskCtx>> tasks;
} os_execute_state;

static ACPI_STATUS thrd_status_to_acpi_status(Thread* status) {
  if (status == nullptr) {
    return AE_ERROR;
  }
  return AE_OK;
}

ACPI_STATUS AcpiTaskThreadStart() {
  os_execute_state.thread =
      Thread::Create("acpi_os_task", AcpiOsExecuteTask, nullptr, DEFAULT_PRIORITY);
  return thrd_status_to_acpi_status(os_execute_state.thread);
}

ACPI_STATUS AcpiTaskThreadTerminate() {
  {
    Guard<Mutex> lock(&os_execute_state.lock);
    os_execute_state.shutdown = true;
  }
  os_execute_state.cond.Broadcast();
  os_execute_state.thread->Join(nullptr, ZX_TIME_INFINITE);
  return AE_OK;
}

static int AcpiOsExecuteTask(void* arg) {
  while (1) {
    ktl::unique_ptr<AcpiOsTaskCtx> task;

    {
      os_execute_state.lock.lock().Acquire();
      while ((task = os_execute_state.tasks.pop_front()) == nullptr) {
        os_execute_state.idle = true;
        // If anything is waiting for the queue to empty, notify it.
        os_execute_state.idle_cond.Signal();

        // If we're waiting to shutdown, do it now that there's no more work
        if (os_execute_state.shutdown) {
          os_execute_state.lock.lock().Release();
          return 0;
        }

        os_execute_state.cond.Wait(&os_execute_state.lock.lock());
      }
      os_execute_state.idle = false;
      os_execute_state.lock.lock().Release();
    }

    task->func(task->ctx);
  }

  return 0;
}

/**
 * @brief Schedule a procedure for deferred execution.
 *
 * @param Type Type of the callback function.
 * @param Function Address of the procedure to execute.
 * @param Context A context value to be passed to the called procedure.
 *
 * @return AE_OK The procedure was successfully queued for execution.
 * @return AE_BAD_PARAMETER The Type is invalid or the Function pointer
 *         is NULL.
 */
ACPI_STATUS AcpiOsExecute(ACPI_EXECUTE_TYPE Type, ACPI_OSD_EXEC_CALLBACK Function, void* Context) {
  if (Function == NULL) {
    return AE_BAD_PARAMETER;
  }

  switch (Type) {
    case OSL_GLOBAL_LOCK_HANDLER:
    case OSL_NOTIFY_HANDLER:
    case OSL_GPE_HANDLER:
    case OSL_DEBUGGER_MAIN_THREAD:
    case OSL_DEBUGGER_EXEC_THREAD:
    case OSL_EC_POLL_HANDLER:
    case OSL_EC_BURST_HANDLER:
      break;
    default:
      return AE_BAD_PARAMETER;
  }

  fbl::AllocChecker ac;
  std::unique_ptr<AcpiOsTaskCtx> task(new (&ac) AcpiOsTaskCtx);
  if (!ac.check()) {
    return AE_NO_MEMORY;
  }
  task->func = Function;
  task->ctx = Context;

  {
    Guard<Mutex> lock(&os_execute_state.lock);
    os_execute_state.tasks.push_back(ktl::move(task));
  }
  os_execute_state.cond.Signal();

  return AE_OK;
}

/**
 * @brief Wait for completion of asynchronous events.
 *
 * This function blocks until all asynchronous events initiated by
 * AcpiOsExecute have completed.
 */
void AcpiOsWaitEventsComplete(void) {
  os_execute_state.lock.lock().Acquire();
  while (!os_execute_state.idle) {
    os_execute_state.idle_cond.Wait(&os_execute_state.lock.lock());
  }
  os_execute_state.lock.lock().Release();
}
