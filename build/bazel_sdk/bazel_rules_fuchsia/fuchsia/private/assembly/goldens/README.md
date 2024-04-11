# Additional Inputs for Build-Time Verification

## Pre-signing Policies

These policy files are intended for use by Scrutiny's pre-signing verifier. They enforce platform-level constraints based on build type (userdebug and user), following the strategy from https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0115_build_types.

These checks are run downstream by the signing server, which maintains an equivalent of the policy as a GCL configuration. The GCL is the source of truth for the policies defined here. Updating the policy files or golden files here are not sufficient to prevent a signing failure.

In order to deduplicate the policy, the signing server should run the Scrutiny verifiers at signing time. The effort to enable this is tracked at b/311208337.

Please consult with the security team if any checks need to be updated or removed.