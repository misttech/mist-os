# Account Handler

## Overview

The Account Handler component exists to manage the state of individual accounts
(and their personae) in the Fuchsia account system. Account Manager creates a
new Account Handler component instance when a client attempts to interact with
an account for the first time. Each Account Handler component instance is
responsible for a single account and cannot access data for other accounts in
the system.

Account Handler orchestrates the provisioning and authentication of an account
and is responsible for mounting storage volumes protected by these
authentication factors (aka account-encrypted storage).

Account Handler does not have access to device-encrypted storage or network
(this reduces the risk of mass exfiltration from account-encrypted storage if
the system were to be compromised). Instead, Account Manager persists the
"pre-authentication state" (information about an account that must be available
before authentication is complete) and passes it to Account Handler over FIDL.
Account Handler stores the "post-authentication state" (information about an
account that is only available after authentication has been completed) as a
file in account-encrypted storage.

Two variants of the Account Handler component are defined, one for persistent
accounts and and one for ephemeral accounts. The ephemeral variant does not
persist account encryption keys and therefore the account-encrypted storage for
an ephemeral account cannot be read once the Account Handler component instance
has been terminated. Currently persistent accounts require a single
authentication mechanisms enrollment while ephemeral accounts do not have any
authentication mechanisms enrolled.

The Account Handler component serves the
`fuchsia.identity.internal.AccountHandlerControl` for communication with
Account Manager. In addition, Account Handler acts as a FIDL server for
`fuchsia.identity.mechanisms.Interaction`, `fuchsia.identity.account.Account`
and `fuchsia.identity.account.Persona`.


## Key Dependencies

- **/identity/lib/account_common** - Account Handler uses error and identifier
  definitions from this crate.
- **/identity/lib/identity_common** - Account Handler uses the TaskGroup type
  from this crate.
- **/identity/lib/storage_manager** - Account Handler uses the file system
  integrations provided by this crate.


## Design

The Account Handler component is composed of several different rust modules
whose responsibilities and key interactions are summarized below:

- **`main`** - This module reads configuration and selects an appropriate
  storage manager implementation before handling control to the
  `account_handler` module. `main` is not unit tested so its scope should remain
  minimal.
- **`account_handler`** - This large module is currently a menagerie of
  everything that did not fit cleanly into some other module. Over time we
  expect to refactor this into more modules with better cohesion, but the scope
  of `account_handler` currently contains:
  - Defining the top level component state
  - Defining the top-level "lifecycle" state machine for the component
  - Defining an sub-state machine for the component once it has been initialized
  - Implementing a `fuchsia.identity.internal.AccountHandlerControl` server
  - Invoking the storage manager library.
- **`storage_lock_state`** - This module manages:
  - the state of the account with respect to being storage-locked, i.e. whether
    or not the underlying storage volume is mounted.
  - enum variants for managing various storage lock states and state
    transitions.
- **`account`** - This module defines the state of a storage-unlocked account,
  uses the `stored_account` module to load and save post-authentication state for
  the account, and implements a `fuchsia.identity.account.Account` server to
  expose information about the account to other components. The `account` module
  also uses the `persona` module to serve requests for personae. No other modules
  should understand the details of a storage-unlocked account.
- **`interaction_lock_state`** - This module manages: 
  - the enum which represents the state of the account with respect to being 
    interaction-locked, i.e. whether or not the account is currently closed
    to interaction because of either a manual lock action or inactivity.
  - enum variants for managing various interaction lock states and state
    transitions.
  - a mechanism for listening for inactivity and automatically moving between
    interaction lock states.
- **`persona`** - This module defines the state of an account persona and
  implements a `fuchsia.identity.account.Persona` server to expose information
  about the persona to other components. No other modules should understand
  personae.
- **`interaction`** - This module implements a FIDL server for the
  `fuchsia.identity.mechanisms.Interaction` protocol, delegating authentication
  and enrollment requests to an authentication mechanism supplied at
  construction. No other modules should understand the details of the
  interaction protocol
- **`pre_auth`** - This module defines the pre-authentication state of an
  account and the methods to serialize into and deserialize from the byte string
  sent over FIDL. Pre-authentication state include the AccountID and the
  information needed to perform authentication. `pre_auth` depends on the
  `wrapped_key` module to define the wrapped volume keys. No other modules
  should understand the serialized form of pre-authentication state.
- **`stored_account`** - This module defines the post-authentication state of an
  account and the methods to to save to and load from storage. No other modules
  should understand how post-authentication state is stored.
- **`wrapped_key`** - This module defines a data structure used inside
  pre-authentication state to store volume encryption keys and provides
  cryptographic operations to wrap and unwrap these keys.
- **`lock_request`** - This module defines a simple wrapper used to communicate
  account lock events (both storage lock and integration lock) received on any
  `Account` channel to the `account_handler` module.
- **`inspect`** - This module defines the data that the component publishes to
  the Inspect diagnostics system.
- **`common`** - This module defines data types that are used widely across
  several other modules.
- **`test_util`** - The module contains helper data types and constants that are
  used widely in the unit test for other modules


## Future Work

The Account Handler is not yet fully complete. In particular the change listener
protocols and local authentication state are not finished.

Currently Account Handler (and the associated FIDL protocol) only handles a
single persona for each account. Eventually we will support the creation and
management of multiple personae.

