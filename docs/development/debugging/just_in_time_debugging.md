# Just In Time Debugging

## Overview

Just In Time Debugging (JITD) is a way for Fuchsia to suspend processes that crash so that
interested parties can debug/process them later. This permits interesting flows such as attaching
zxdb to a program that crashed overnight, when the debugger was not attached/running.

This is done by storing process in exceptions in a special place called the "Process Limbo". This
place will keep those processes suspended until some other agent comes and releases them.

See [Implementation](#implementation) for more details about how it works.

## How to enable it

One of the great benefits of the Process Limbo is to be able to catch crashing processes in the
wild, without the need to have already running debuggers. This is specially useful for situations
where the debugger *cannot* be running, such as driver startup. For such cases, having an active
Process Limbo can provide an invaluable source of debugging information.

There are two ways of enabling the Process Limbo:

### Manual activation

There's an ffx plugin that permits the user to query the current state of the limbo:

```
$ ffx debug limbo --help
Usage: ffx debug limbo <command> [<args>]

control the process limbo on the target

Options:
  --help            display usage information

Commands:
  status            query the status of the process limbo.
  enable            enable the process limbo. It will now begin to capture
                    crashing processes.
  disable           disable the process limbo. Will free any pending processes
                    waiting in it.
  list              lists the processes currently waiting on limbo. The limbo
                    must be active.
  release           release a process from limbo. The limbo must be active.

See 'ffx help <command>' for more information on a specific command.
```

### Enable on startup

Manual activation works only if you have a way to send commands to the system. But some development
environments run software earlier that the user can interact with (or run a debugger). Drivers are a
good example of this. For those cases, having the Process Limbo active from the start lets you catch
driver crashes as they occur while the driver is spinning up, which is normally the hardest part to
debug.

In order to do this, there is a configuration that has to be set into the build:

```
fx set <YOUR CONFIG> --with-base //src/developer/forensics:exceptions_enable_jitd_on_startup
```

Or add this label to the `base_package_labels` in your build args. You can still use the Process
Limbo CLI tool to disable and manipulate the limbo afterwards. Then you will need to push an update
to your device for this to take an effect.

NOTE: Driver initialization is finicky and freezing crashing process can leave the system in an
undefined state and "hang" it, so your mileage may vary when using this feature, especially for very
early drivers.

## How to use it

### zxdb

The main user of JITD is zxdb, which is able to attach to a process waiting in the limbo. When
starting zxdb, it will automatically attach to processes waiting in limbo:

```
$ ffx debug connect
Connecting (use "disconnect" to cancel)...
Connected successfully.
👉 To get started, try "status" or "help".
Processes attached from limbo:
  48487: crasher
Type "detach <pid>" to send back to Process Limbo if attached,
type "detach <pid>" again to terminate the process if not attached, or
type "process <process context #> kill" to terminate the process if attached.
See "help jitd" for more information on Just-In-Time-Debugging.
Process "crasher" (48487) crashed and has been automatically attached.
Type "status" for more information.
Attached Process 1 state=Running koid=48487 name=crasher component=sshd-host.cm
Loading 9 modules for crasher Done.
   23
   24 int blind_write(volatile unsigned int* addr) {
 ▶ 25   *addr = 0xBAD1DEA;
   26   return 0;
   27 }
════════════════════════════════════════════════════════════════════════════
 Data fault writing address 0x0 (translation fault level 2) (second chance)
════════════════════════════════════════════════════════════════════════════
 Process 1 (koid=48487) thread 1 (koid=48489)
 Faulting instruction: 0x642ff060

🛑 blind_write(volatile unsigned int*) • crasher.c:25

[zxdb] thread
  # state                koid name
▶ 1 Blocked (Exception) 58692 initial-thread

[zxdb] frame
▶ 0 blind_write(…) • crasher.c:25
  1 main(…) • crasher.c:356
  2…4 «libc startup» (-r expands)

[zxdb] list
   20   int (*func)(volatile unsigned int*);
   21   const char* desc;
   22 } command_t;
   23
   24 int blind_write(volatile unsigned int* addr) {
 ▶ 25   *addr = 0xBAD1DEA;
   26   return 0;
   27 }
   28
   29 int blind_read(volatile unsigned int* addr) { return (int)(*addr); }
   30
   31 int blind_execute(volatile unsigned int* addr) {
   32   void (*func)(void) = (void*)addr;
   33   func();
   34   return 0;
   35 }
```

Within zxdb you can also do `help jitd` to get more information about how to use it.

### Process Limbo FIDL Service

The Process Limbo presents itself as a FIDL service, which is what the Process Limbo CLI tool and
zxdb use. The FIDL protocol is defined in `//sdk/fidl/fuchsia.exception/process_limbo.fidl`.

## Implementation

### Crash Service

When a process throws an exception, Zircon will generate an associated `exception handle`. It will
then look if there are any listeners in any associated exception channels that might be interested
in handling that exception. That is how debuggers such as zxdb get the exceptions from running
processes. See [the exceptions handling](/docs/concepts/kernel/exceptions.md) for more details.

But when there are no more exception handlers left, either because there weren't any or they all
decided to pass on handling it, the root job has an exclusive handler called `crashsvc`. Once an
exception has reached the Crash Service, it is understood that it has "crashed" and that no program
was able to handle it. The Crash Service will then dump the crashing stack trace to the logs and
pass the exception over to the `Exception Broker`.

### Exception Broker

The Exception Broker is in charge of deciding what is to be done with a crashing exception,
depending on the actual system configuration. It might decide to create a minidump file and dump a
crash report, send the exception over to the Process Limbo or kill the process.

The Exception Broker is aware of the Process Limbo and whether it is active or not. When it receives
an exception, it will check whether the Process Limbo is enabled. If so, it will pass the exception
handle over to it. This is the same Process Limbo exposed by the FIDL service.
