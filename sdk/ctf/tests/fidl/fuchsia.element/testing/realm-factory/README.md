# Archivist test realm factory

The archivist test realm factory is a component that builds a test realm
with an archivist and one or more [puppet] components inside.

## Puppet components

The puppet component is a component that emits logs and inspect data that are
read by archvist. The puppet only emits what the test suite requests.

The test realm can contain multiple puppet components. By default, no puppets
are added to the test realm. A test must specify each puppet's component name
when creating the realm. This name will become part of the puppet's moniker.

Each puppet is controlled through the protocol [fuchsia.archivist.test.Puppet].
To allow the test to connect to a specific puppet in a realm containing many
puppets, each puppet's protocol is exposed as the alias
`fuchsia.archivist.test.Puppet.{puppet_name}`. For example, a test can connect
to a puppet named `child_a` by connecting to
`fuchsia.archivist.test.Puppet.child_a`. This means the puppet's name must be
a valid FIDL object name as specified in:
<https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_names>.

[puppet]: //src/diagnostics/archivist/testing/puppet
[fuchsia.archivist.test.Puppet]: //src/diagnostics/archivist/testing/fidl/BUILD.gn
