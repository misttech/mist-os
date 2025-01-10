// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_POWER_SUPPORT_H_
#define LIB_DRIVER_POWER_CPP_POWER_SUPPORT_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/driver/power/cpp/types.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fit/function.h>
#include <lib/zx/event.h>
#include <lib/zx/handle.h>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

/// Collection of helpers for driver authors working with the power framework.
/// The basic usage model is
///   * use `fuchsia.hardware.platform.device/Device.GetPowerConfiguration` to
///     retrieve the config supplied by the board driver.
///   * For each power element in the driver's config
///       - Call `PowerAdapter::GetDependencyTokens` to get the element's
///         parents' access tokens.
///       - Calling `PowerAdapter::AddElement` and supplying the configuration,
///         token set from `GetDependencyTokens` and any access tokens the
///         driver needs to declare.
namespace fdf_power {

enum class Error : uint8_t {
  /// The power configuration appears to be invalid. A non-exhaustive list of
  /// possible reasons is it contained no elements, the element definition
  /// appears malformed, or other reasons.
  INVALID_ARGS,
  /// A general I/O error happened which we're not sure about. This should be
  /// a rare occurrence and typically more specific errors should be returned.
  IO,
  /// The configuration has a dependency, but we couldn't get access to the
  /// tokens for it. Maybe a parent didn't offer something expected or SAG
  /// didn't make something available.
  DEPENDENCY_NOT_FOUND,
  /// No token services capability available, maybe it wasn't routed?
  TOKEN_SERVICE_CAPABILITY_NOT_FOUND,
  /// An unexpected error occurred listing service instances.
  READ_INSTANCES,
  /// We were able to access the token service capability, but no instances
  /// were available. Did the parents offer any?
  NO_TOKEN_SERVICE_INSTANCES,
  /// Requesting a token from the provider protocol failed. Maybe the token
  /// provider is not implemented correctly?
  TOKEN_REQUEST,
  /// Couldn't access the capability for System Activity Governor tokens.
  ACTIVITY_GOVERNOR_UNAVAILABLE,
  /// Request to System Activity Governor returned an error.
  ACTIVITY_GOVERNOR_REQUEST,
  /// fuchsia.power.broker/Topology could not be connected to.
  TOPOLOGY_UNAVAILABLE,
  /// The power configuration could not be retrieved.
  CONFIGURATION_UNAVAILABLE,
  /// Could not access the CpuElementManager capability.
  CPU_ELEMENT_MANAGER_UNAVAILABLE,
  /// There was an error making a request to the CpuElementManager protocol.
  CPU_ELEMENT_MANAGER_REQUEST,
};

// Convenience method that provide an approximate mapping to Zircon error values.
zx::error<int> ErrorToZxError(Error e);

enum class ElementRunnerError : uint8_t {
  /// Maps to fuchsia.power.broker/RequiredLevelError::INTERNAL
  REQUIRED_LEVEL_INTERNAL,
  /// Maps to fuchsia.power.broker/RequiredLevelError::NOT_AUTHORIZED
  REQUIRED_LEVEL_NOT_AUTHORIZED,
  /// Maps to fuchsia.power.broker/RequiredLevelError::UNKNOWN
  REQUIRED_LEVEL_UNKNOWN,
  /// fuchsia.power.broker/RequiredLevelError has a value we don't recognize
  REQUIRED_LEVEL_UNEXPECTED,
  /// The fuchsia.power.broker/RequiredLevel channel closed
  REQUIRED_LEVEL_TRANSPORT_PEER_CLOSED,
  /// The fuchsia.power.broker/RequiredLevel had a FIDL transport error other
  /// than closed.
  REQUIRED_LEVEL_TRANSPORT_OTHER,
  /// Maps to fuchsia.power.broker/CurrentLevelError::NOT_AUTHORIZED
  CURRENT_LEVEL_NOT_AUTHORIZED,
  /// fuchsia.power.broker/CurrentLevelError has a value we don't recognize
  CURRENT_LEVEL_UNEXPECTED,
  /// The fuchsia.power.broker/CurrentLevel channel closed
  CURRENT_LEVEL_TRANSPORT_PEER_CLOSED,
  /// The fuchsia.power.broker/CurrentLevel had a FIDL transport error other
  /// than closed.
  CURRENT_LEVEL_TRANSPORT_OTHER,
  /// The level change callback returned an error
  LEVEL_CHANGE_CALLBACK,
};

/// Runs a power element.
///
/// Once |RunPowerElement| is called, this object listens for new levels
/// reported to it via |RequiredLevel.Watch|, calls the provided
/// |level_change_callback|, and reports the level returned by that callback
/// via |CurrentLevel.Update|. This object stops running the power element if
/// an error occurs and reports the error via |error_handler|. This class is
/// not thread-safe and should created on a thread with a synchronized
/// dispatcher and given a pointer to that same dispatcher. Since the callbacks
/// happen on the same dispatcher, blocking the callbacks blocks runnng the
/// element. Calls to |SetLevel| do not trigger a |level_change_callback|
/// invocation.
class ElementRunner {
 public:
  ElementRunner(fidl::ClientEnd<fuchsia_power_broker::RequiredLevel> required_level,
                fidl::ClientEnd<fuchsia_power_broker::CurrentLevel> current_level,
                fit::function<fit::result<zx_status_t, uint8_t>(uint8_t)> level_change_callback,
                fit::function<void(ElementRunnerError)> error_handler,
                async_dispatcher_t* dispatcher) {
    on_error_ = std::move(error_handler);
    on_level_change_ = std::move(level_change_callback);
    required_level_client_ =
        fidl::Client<fuchsia_power_broker::RequiredLevel>(std::move(required_level), dispatcher);
    current_level_client_ =
        fidl::Client<fuchsia_power_broker::CurrentLevel>(std::move(current_level), dispatcher);
  }

  /// Runs the power element asynchronously. The object continues running the
  /// power element until an error occurs or the object is destroyed. Running
  /// the element *and* making callbacks via |level_change_callback| and
  /// |error_handler| are done on the object's dispatcher.
  ///
  /// The object listens for new levels, calls |level_change_callback| when one
  /// is received, reports the power level returned from |level_change_callback|
  /// via the |current_level| channel provided to the constructor, and calls
  /// |error_handler| if an error occurs.
  ///
  /// After |error_handler| is called, this object stops running the element.
  /// |RunPowerElement| can then be called again to continue running it.
  void RunPowerElement();

  /// Sets the level of the element via the |CurrentLevel| channel provided to
  /// the constructor. The call returns immediately and the result is delivered
  /// to |callback| on the |dispatcher| passed to the constructor.
  void SetLevel(
      uint8_t level,
      fit::function<void(
          fit::result<fidl::ErrorsIn<fuchsia_power_broker::CurrentLevel::Update>, zx_status_t>)>
          callback);

 private:
  fidl::Client<fuchsia_power_broker::RequiredLevel> required_level_client_;
  fidl::Client<fuchsia_power_broker::CurrentLevel> current_level_client_;
  fit::function<fit::result<zx_status_t, uint8_t>(uint8_t)> on_level_change_;
  fit::function<void(ElementRunnerError)> on_error_;
};

inline fit::result<zx_status_t, uint8_t> default_level_changer(uint8_t level) {
  return fit::ok(level);
}

/// |LeaseHelper| wraps the collection of channels that represents a power
/// element. When used with |CreateLeaseHelper| the caller just supplies
/// the level of the required element(s) and dependency token(s). The caller
/// does not need to create an element it manages to get the dependencies
/// to the level it needs, instead it can use the |LeaseHelper| returned from
/// |CreateLeaseHelper|.
///
/// The |LeaseHelper| runs an element internally and exposes a convenient
/// interface, |AcquireLease| which leases the wrapped element at the "on"
/// state and consequentally drives the dependencies to their desired state.
class LeaseHelper {
 public:
  /// Creates a |LeaseHelper| running on the supplied |dispatcher|. The lease
  /// is **not** active when the constructor returns, use |AcquireLease| for.
  /// this. Blocking the dispatcher while waiting for the callback from
  /// |AcquireLease| will result in a deadlock.
  ///
  /// |error_callback| is called if |LeaseHelper| encounters an error running
  /// its wrapped power element. If the error handler is invokes, the
  /// |LeaseHelper| should be dropped and a new one created.
  LeaseHelper(fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control,
              fidl::ClientEnd<fuchsia_power_broker::Lessor> lessor,
              fidl::ClientEnd<fuchsia_power_broker::RequiredLevel> required_level,
              fidl::ClientEnd<fuchsia_power_broker::CurrentLevel> current_level,
              async_dispatcher_t* dispatcher, fit::function<void()> error_callback)
      : dispatcher_(dispatcher),
        element_control_(fidl::Client<fuchsia_power_broker::ElementControl>(
            std::move(element_control), dispatcher_)),
        lessor_(fidl::Client<fuchsia_power_broker::Lessor>(std::move(lessor), dispatcher_)),
        runner_(
            std::move(required_level), std::move(current_level), default_level_changer,
            [callback = std::move(error_callback)](ElementRunnerError err) { callback(); },
            dispatcher_) {
    runner_.RunPowerElement();
  }

  /// Trigger the lease acquisition. The lease is **created**, but not active,
  /// when |callback| is invoked. The lease is active when
  /// `fuchsia.power.broker/LeaseControl` channel given to |callback| reports
  /// `fuchsia.power.broker/LeaseStatus::Satisfied` from
  /// `fuchsia.power.broker/LeaseControl.WatchStatus()`.
  ///
  /// Release the lease by closing the `LeaseControl` channel. |AcquireLease|
  /// can be called more than once and creates a lease for each call.
  void AcquireLease(
      fit::function<void(fidl::Result<fuchsia_power_broker::Lessor::Lease>&)> callback);

 private:
  async_dispatcher_t* dispatcher_;
  fidl::Client<fuchsia_power_broker::ElementControl> element_control_;
  fidl::Client<fuchsia_power_broker::Lessor> lessor_;
  ElementRunner runner_;
};

class LeaseDependency {
 public:
  std::vector<fuchsia_power_broker::PowerLevel> levels_by_preference;
  fuchsia_power_broker::DependencyToken token;
  fuchsia_power_broker::DependencyType type;
};

/// Uses the provided namespace to add the power elements described in
/// |power_configs| to the power topology and returns corresponding
/// `ElementDesc` instances.
/// This function:
///     * Retrieves the tokens of any dependencies via
///       `fuchsia.hardware.power/PowerTokenProvider` instances
///     * Adds the power element via `fuchsia.power.broker/Topology`
///
/// In effect, this function converts the provided |power_configs| into their
/// corresponding `ElementDesc` objects and returns them.
fit::result<Error, std::vector<ElementDesc>> ApplyPowerConfiguration(
    const fdf::Namespace& ns, cpp20::span<PowerElementConfiguration> power_configs);

/// Create a lease based on the set of dependencies represented by
/// |dependencies|. When the lease is fulfilled those dependencies will be at
/// the level specified. The lease is **not** active when this function returns,
/// use |LeaseHelper::AcquireLease| to trigger lease activation.
///
/// The |dispatcher| passed in is used to run a power element, so blocking
/// the dispatcher while acquiriring a lease with |LeaseHelper::AcquireLease|
/// will result in a deadlock.
///
/// |error_callback| is **not** invoked if |LeaseHelper| creation fails,
/// instead it is called if the running the internal power element encounters
/// an error. If this error occurs, future lease acquisitions will likely fail
/// and the |LeaseHelper| should be replaced with a new instance.
///
/// RETURN VALUES
/// On error returns a tuple representng whether it was a FIDL error or a
/// protocol error. If the |fidl::Status| is not ZX_OK, this was a FIDL error,
/// and the second member of the tuple will be `nullopt`. Otherwise, this is a
/// protocol error from adding the power element that the direct lease wraps
/// and will be an error value from `fuchsia.power.broker/Topology.AddElement`.
/// On success returns a |LeaseHelper|.
fit::result<std::tuple<fidl::Status, std::optional<fuchsia_power_broker::AddElementError>>,
            std::unique_ptr<LeaseHelper>>
CreateLeaseHelper(const fidl::ClientEnd<fuchsia_power_broker::Topology>& topology,
                  std::vector<LeaseDependency> dependencies, std::string lease_name,
                  async_dispatcher_t* dispatcher, fit::function<void()> error_callback);

/// Given a `PowerElementConfiguration` from driver framework, convert this
/// into a set of Power Broker's `LevelDependency` objects. The map is keyed
/// by the name of the parent/dependency.
///
/// If the `PowerElementConfiguration` expresses no dependencies, we return an
/// empty map.
///
/// NOTE: The `requires_token` of each of the `LevelDependency` objects is
/// **not** populated and must be filled in before providing this map to
/// `AddElement`.
///
/// Error returns:
///   - Error::INVALID_ARGS if `element_config` is missing fields, for example
///     if a level dependency doesn't have a parent level.
fit::result<Error, ElementDependencyMap> LevelDependencyFromConfig(
    const PowerElementConfiguration& element_config);

/// Given a `PowerElementConfiguration` from driver framework, convert this
/// into a set of Power Broker's `PowerLevel` objects.
///
/// If the `PowerElementConfiguration` expresses no levels, we return an
/// empty vector.
std::vector<fuchsia_power_broker::PowerLevel> PowerLevelsFromConfig(
    PowerElementConfiguration element_config);

/// For the Power Element represented by `element_config`, get the tokens for
/// the element's dependencies (ie. "parents") from
/// `fuchsia.hardware.power/PowerTokenProvider` instances in `ns`.
///
/// If the power element represented by `element_config` has no dependencies,
/// this function returns an empty set. If any dependency's token can not be
/// be retrieved we return an error.
/// Error returns:
///   - `Error::INVALID_ARGS` if the element_config appears invalid
///   - `Error::IO` if there is a communication failure when talking to a
///      service or a protocol required to get a token.
///   - `Error::DEPENDENCY_NOT_FOUND` if a token for a required dependency is
///     not available.
fit::result<Error, TokenMap> GetDependencyTokens(const fdf::Namespace& ns,
                                                 const PowerElementConfiguration& element_config);

/// For the Power Element represented by `element_config`, get the tokens for
/// the
/// element's dependencies (ie. "parents") from
/// `fuchsia.hardware.power/PowerTokenProvider` instances in `svcs_dir`.
/// `svcs_dir` should contain an entry for
/// `fuchsia.hardware.power/PowerTokenService`.
///
/// Returns a set of tokens from services instances found in `svcs_dir`. If
/// the power element represented by `element_config` has no dependencies, this
/// function returns an empty set. If any dependency's token can not be
/// be retrieved we return an error.
/// Error returns:
///   - `Error::INVALID_ARGS` if the element_config appears invalid
///   - `Error::IO` if there is a communication failure when talking to a
///      service or a protocol required to get a token.
///   - `Error::DEPENDENCY_NOT_FOUND` if a token for a required dependency is
///     not available.
fit::result<Error, TokenMap> GetDependencyTokens(const PowerElementConfiguration& element_config,
                                                 fidl::ClientEnd<fuchsia_io::Directory> svcs_dir);

/// Call `AddElement` on the `power_broker` channel passed in.
/// This function uses the `config` and `tokens` arguments to properly construct
/// the call to `fuchsia.power.broker/Topology.AddElement`. Optionally callers
/// can pass in tokens to be registered for granting assertive and opportunistic
/// dependency access on the created element.
///
/// Error
///   - Error::DEPENDENCY_NOT_FOUND if there is a dependency specified by
///     `config` which is to found in `tokens`.
///   - Error::INVALID_ARGS if `config` appears to be invalid, we fail to
///     duplicate a token and therefore assume it must have been invalid, or
///     the call to power broker fails for any reason *other* than a closed
///     channel.
fit::result<Error> AddElement(
    const fidl::ClientEnd<fuchsia_power_broker::Topology>& power_broker,
    const PowerElementConfiguration& config, TokenMap tokens,
    const zx::unowned_event& assertive_token, const zx::unowned_event& opportunistic_token,
    std::optional<std::pair<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>,
                            fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>>>
        level_control,
    std::optional<fidl::ServerEnd<fuchsia_power_broker::Lessor>> lessor,
    std::optional<fidl::ServerEnd<fuchsia_power_broker::ElementControl>> element_control,
    std::optional<fidl::UnownedClientEnd<fuchsia_power_broker::ElementControl>>
        element_control_client);

/// Call `AddElement` on the `power_broker` channel passed in.
/// This function uses `ElementDescription` passed in to make the proper call
/// to `fuchsia.power.broker/Topology.AddElement`. See `ElementDescription` for
/// more information about what fields are inputs to `AddElement`.
///
/// Error
///   - Error::DEPENDENCY_NOT_FOUND if there is a dependency specified by
///     `config` which is to found in `tokens`.
///   - Error::INVALID_ARGS if `config` appears to be invalid, we fail to
///     duplicate a token and therefore assume it must have been invalid, or
///     the call to power broker fails for any reason *other* than a closed
///     channel.
fit::result<Error> AddElement(fidl::ClientEnd<fuchsia_power_broker::Topology>& power_broker,
                              ElementDesc& description);
}  // namespace fdf_power

#endif

#endif  // LIB_DRIVER_POWER_CPP_POWER_SUPPORT_H_
