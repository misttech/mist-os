// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUG_AGENT_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUG_AGENT_H_

#include <zircon/types.h>

#include <cstdint>
#include <map>
#include <memory>

#include "gtest/gtest_prod.h"
#include "src/developer/debug/debug_agent/breakpoint.h"
#include "src/developer/debug/debug_agent/debug_agent_observer.h"
#include "src/developer/debug/debug_agent/debugged_job.h"
#include "src/developer/debug/debug_agent/debugged_process.h"
#include "src/developer/debug/debug_agent/filter.h"
#include "src/developer/debug/debug_agent/job_exception_observer.h"
#include "src/developer/debug/debug_agent/limbo_provider.h"
#include "src/developer/debug/debug_agent/remote_api.h"
#include "src/developer/debug/debug_agent/remote_api_adapter.h"
#include "src/developer/debug/ipc/message_writer.h"
#include "src/developer/debug/ipc/protocol.h"
#include "src/developer/debug/ipc/records.h"
#include "src/developer/debug/shared/logging/logging.h"
#include "src/developer/debug/shared/stream_buffer.h"
#include "src/lib/fxl/macros.h"
#include "src/lib/fxl/memory/weak_ptr.h"
#include "src/lib/fxl/observer_list.h"

namespace debug_agent {

class SystemInterface;

// Main state and control for the debug agent.
class DebugAgent : public RemoteAPI, public Breakpoint::ProcessDelegate, public debug::LogBackend {
 public:
  // A MessageLoopZircon should already be set up on the current thread.
  //
  // The stream must outlive this class. It will be used to send data to the
  // client. It will not be read (that's the job of the provider of the
  // RemoteAPI).
  explicit DebugAgent(std::unique_ptr<SystemInterface> system_interface);

  fxl::WeakPtr<DebugAgent> GetWeakPtr();

  SystemInterface& system_interface() { return *system_interface_; }
  const std::map<uint32_t, Breakpoint>& breakpoints() { return breakpoints_; }

  // Reflects the state of a debug_ipc connection.
  bool is_connected() const { return buffered_stream_ && buffered_stream_->IsValid(); }

  // Wire |stream| up to |adapter_| and then pass it to |Connect|.
  void TakeAndConnectRemoteAPIStream(std::unique_ptr<debug::BufferedStream> stream);
  // Take ownership of |stream| and start listening.
  void Connect(std::unique_ptr<debug::BufferedStream> stream);
  void Disconnect();

  void RemoveDebuggedProcess(zx_koid_t process_koid);

  Breakpoint* GetBreakpoint(uint32_t breakpoint_id);
  void RemoveBreakpoint(uint32_t breakpoint_id);

  enum class ProcessChangedHow {
    kStarting,
    kNameChanged,
  };
  void OnProcessChanged(ProcessChangedHow how, std::unique_ptr<ProcessHandle> process);

  // Notified by ComponentManager.
  void OnComponentStarted(const std::string& moniker, const std::string& url, zx_koid_t job_koid);
  void OnComponentExited(const std::string& moniker, const std::string& url);
  void OnTestComponentExited(const std::string& url);

  void InjectProcessForTest(std::unique_ptr<DebuggedProcess> process);

  DebuggedJob* GetDebuggedJob(zx_koid_t koid);
  DebuggedProcess* GetDebuggedProcess(zx_koid_t koid);
  DebuggedThread* GetDebuggedThread(const debug_ipc::ProcessThreadId& id);

  // Returns the exception handling strategy for a given type.
  debug_ipc::ExceptionStrategy GetExceptionStrategy(debug_ipc::ExceptionType type);

  // Suspends all threads of all attached processes. If given the process/thread will be excepted
  // from the suspend (they must both be either specified or ZX_KOID_INVALID).
  //
  // The affected process/thread koid pairs are returned. Any threads already in a client suspend
  // will not be affected.
  std::vector<debug_ipc::ProcessThreadId> ClientSuspendAll(
      zx_koid_t except_process = ZX_KOID_INVALID, zx_koid_t except_thread = ZX_KOID_INVALID);

  // Clear all state and release all attached processes.
  void ClearState();

  // Send notification to the client.
  template <typename NotifyType>
  void SendNotification(const NotifyType& notify) {
    std::vector<char> serialized = debug_ipc::Serialize(notify, ipc_version_);
    if (is_connected() && !serialized.empty())
      buffered_stream_->stream().Write(std::move(serialized));

    for (auto& observer : observers_) {
      observer.OnNotification(notify);
    }
  }

  // RemoteAPI implementation.
  uint32_t GetVersion() override { return ipc_version_; }
#define FN(type) \
  void On##type(const debug_ipc::type##Request& request, debug_ipc::type##Reply* reply) override;

  FOR_EACH_REQUEST_TYPE(FN)
#undef FN

  // Implements |LogBackend|.
  void WriteLog(debug::LogSeverity severity, const debug::FileLineFunction& location,
                std::string log) override;

  void AddObserver(DebugAgentObserver* observer) { observers_.AddObserver(observer); }

  // Note this is a potential exit point. If there are no more observers after |observer| has been
  // removed, and no debug_ipc client is connected, the message loop will be shut down and this
  // agent will exit.
  void RemoveObserver(DebugAgentObserver* observer);

  const std::map<zx_koid_t, std::unique_ptr<DebuggedProcess>>& GetAllProcesses() const {
    return procs_;
  }

 private:
  FRIEND_TEST(DebugAgentTests, Kill);

  // Breakpoint::ProcessDelegate implementation ----------------------------------------------------

  debug::Status RegisterBreakpoint(Breakpoint* bp, zx_koid_t process_koid,
                                   uint64_t address) override;
  void UnregisterBreakpoint(Breakpoint* bp, zx_koid_t process_koid, uint64_t address) override;
  void SetupBreakpoint(const debug_ipc::AddOrChangeBreakpointRequest& request,
                       debug_ipc::AddOrChangeBreakpointReply* reply);

  debug::Status RegisterWatchpoint(Breakpoint* bp, zx_koid_t process_koid,
                                   const debug::AddressRange& range) override;
  void UnregisterWatchpoint(Breakpoint* bp, zx_koid_t process_koid,
                            const debug::AddressRange& range) override;

  // Process/Thread Management -----------------------------------------------------------------

  debug::Status AddDebuggedProcess(DebuggedProcessCreateInfo&&, DebuggedProcess** added);
  debug::Status AddDebuggedJob(DebuggedJobCreateInfo&& create_info, DebuggedJob** added);

  // Returns true if we're attached to the standard exception channel of |parent| or any ancestor of
  // |parent| under the root job. The variant that takes a ProcessHandle will start with the
  // process's parent.
  bool IsAttachedToParentOrAncestorOf(zx_koid_t parent);
  bool IsAttachedToParentOrAncestorOf(const ProcessHandle* process);

  // Attempts to attach to the given process and sends a AttachReply message
  // to the client with the result.
  debug::Status AttachToLimboProcess(zx_koid_t process_koid, debug_ipc::AttachReply* reply);
  debug::Status AttachToExistingProcess(zx_koid_t process_koid,
                                        const debug_ipc::AttachConfig& config,
                                        debug_ipc::AttachReply* reply);
  debug::Status AttachToRootJob();
  debug::Status AttachToExistingJob(zx_koid_t job_koid, const debug_ipc::AttachConfig& config,
                                    debug_ipc::AttachReply* reply);

  void LaunchProcess(const debug_ipc::RunBinaryRequest&, debug_ipc::RunBinaryReply*);

  // Process Limbo ---------------------------------------------------------------------------------

  void OnProcessEnteredLimbo(const LimboProvider::Record& record);

  // Warning: Be very careful when using the returned vector and modifying |filters_|. If an action
  // causes |filters_| to be reallocated, the returned pointers will be invalidated.
  std::vector<const Filter*> GetMatchingFiltersForComponentInfo(const std::string& moniker,
                                                                const std::string& url) const;

  // Members ---------------------------------------------------------------------------------------

  std::unique_ptr<RemoteAPIAdapter> adapter_;

  std::vector<Filter> filters_;

  std::unique_ptr<SystemInterface> system_interface_;

  std::unique_ptr<debug::BufferedStream> buffered_stream_;

  fxl::ObserverList<DebugAgentObserver> observers_;
  uint32_t ipc_version_ = 0;

  DebuggedJob* root_job_;
  std::map<zx_koid_t, std::unique_ptr<DebuggedProcess>> procs_;
  std::map<zx_koid_t, std::unique_ptr<DebuggedJob>> jobs_;

  // Processes obtained through limbo do not have the ZX_RIGHT_DESTROY right, so cannot be killed
  // by the debugger. Instead what we do is detach from them and mark those as "waiting to be
  // killed". When they re-enter the limbo, the debugger will then detect that is one of those
  // processes and release it from limbo, effectively killing it.
  //
  // NOTE(01/2020): The reason for this is because the limbo gets the exception from crashsvc,
  //                which has an exception channel upon the root job handle. Exceptions obtained
  //                through an exception channel will hold the same rights as the origin handle (the
  //                root job one in this case). Now, the root job handle pwrbtn-monitor receives
  //                doesn't have the ZX_RIGHT_DESTROY right, as you don't really want to be able to
  //                kill the root job. This means that all the handles obtained through an exception
  //                will not have the destroy right, thus making the debugger jump through this
  //                hoop.
  //
  //                See src/bringup/bin/pwrbtn-monitor/crashsvc.cc for more details.
  //
  //                Note that if the situation changes and the pwrbtn-monitor actually gets destroy
  //                rights on the exception channel, that situation will seamlessly work here. This
  //                is because the debug agent will only track "limbo killed processes" if trying to
  //                kill results in ZX_ERR_ACCESS_DENIED *and* they came from limbo. If the limbo
  //                process handles now have destroy rights, killing them will work, thus skipping
  //                this dance.
  std::set<zx_koid_t> killed_limbo_procs_;

  std::map<uint32_t, Breakpoint> breakpoints_;

  // Normally the debug agent would be attached to the root job or the
  // component root job and give the client the koid. This is a job koid needed
  // to be able to create an invisible filter to catch the newly started
  // component. This will be 0 if not attached to such a job.
  // TODO(donosoc): Hopefully we could get the created job for the component
  //                so we can only filter on that.
  zx_koid_t attached_root_job_koid_ = 0;

  std::map<debug_ipc::ExceptionType, debug_ipc::ExceptionStrategy> exception_strategies_;

  fxl::WeakPtrFactory<DebugAgent> weak_factory_;

  FXL_DISALLOW_COPY_AND_ASSIGN(DebugAgent);
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUG_AGENT_H_
