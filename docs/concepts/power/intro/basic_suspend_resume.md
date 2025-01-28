# Basic Usage


## Taking action on system suspend or resume

A component may want to take some action when the CPU suspends or resumes. The
component can use an [`ActivityGovernorListener`][act_gov_list] to observe these
transitions. The listener registers by calling
[`fuchsia.power.system/ActivityGovernor.RegisterListener`][register_listener].
The system will not suspend until after all listeners reply to
[`ActivityGovernorListener.OnSuspendStarted`][suspend_started]. On system
resume, System Activity Governor will not raise the level of its
`ApplicationActivity` element until all listeners reply to
[`ActivityGovernorListener.OnResume`][resume].

[act_gov_list]: https://cs.opensource.google/fuchsia/fuchsia/+/39b9a242c6e2b09731a426cdcf9f1353206fd034:sdk/fidl/fuchsia.power.system/system.fidl;l=174
[register_listener]: https://cs.opensource.google/fuchsia/fuchsia/+/39b9a242c6e2b09731a426cdcf9f1353206fd034:sdk/fidl/fuchsia.power.system/system.fidl;l=282
[resume]: https://cs.opensource.google/fuchsia/fuchsia/+/39b9a242c6e2b09731a426cdcf9f1353206fd034:sdk/fidl/fuchsia.power.system/system.fidl;l=181
[suspend_started]: https://cs.opensource.google/fuchsia/fuchsia/+/39b9a242c6e2b09731a426cdcf9f1353206fd034:sdk/fidl/fuchsia.power.system/system.fidl;l=187
