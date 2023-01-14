# End to end test framework

Fuchsia's Python End-to-end multi-device testing framework is located here.

This framework can be used for authoring host-driven E2E tests that interact
with 1 or more Fuchsia devices. The framework can be extended to work with
non Fuchsia devices as well.

This framework aims to provide a uniform test writing experience to all
users and exposes conveninece methods for Fuchsia device interactions that
are ergonomic and robust; with the ultimate goal of providing scriptable
host-side FIDL access to a Fuchsia device-under-test.