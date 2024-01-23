# Configuration capabilities

Configuration capabilities allow components to define, route, and use configuration
values. A component that uses a configuration capability will see that value appear
in its [structured configuration][structured-configuration].

## Defining configuration capabilities

To define a configuration capability, a component must add a `capabilities` declaration
for it:

```json5
{
    capabilities: [
        {
            config: "fuchsia.config.MyBool",
            type: "bool",
            value: true,
        },
    ],
}
```

This defines the capability `fuchsia.config.MyBool` of type `bool` with the value of `true`.
Please see the [capabilities reference][docs-capability] for more information
on the supported fields for `type` and `value`.

## Routing configuration capabilities {#route}

Components route configuration capabilities by [exposing](#expose) them to their
parent or [offering](#offer) them to their children.

For more details on how the framework routes component capabilities,
see [capability routing][capability-routing].

### Exposing {#expose}

Exposing a configuration capability gives the component's parent access to that
capability:

```json5
{
    expose: [
        {
            config: "fuchsia.config.MyBool",
            from: "self",
        },
    ],
}
```

You may optionally specify:

* [`as`](#renaming)
* [`availability`](#availability)

### Offering {#offer}

Offering a configuration capability gives a child component access to that
capability:

```json5
{
    offer: [
        {
            config: "fuchsia.config.MyBool",
            from: "parent",
            to: [ "#child-a", "#child-b" ],
        },
    ],
}
```

You may optionally specify:

* [`as`](#renaming)
* [`availability`](#availability)

### Using configuration capabilities {#use}

To use a configuration capability, the component must add a `use` definition that links the
capability to a structured configuration value. Then, the component will be able to read its
structured configuration at runtime to see the value of the capability.

Consider a component with the following structured configuration block:
```json5
{
   config: {
        say_hello: { type: "bool" },
    },
}
```


To use a configuration capability, add a `use` declaration for it:

```json5
{
    use: [
        {
            config: "fuchsia.config.MyBool",
            config_key: "say_hello",
            from: "parent",
        },
    ],
}
```

When a component reads the `say_hello` field of its structured configuration, it will be
receiving the value of `fuchsia.config.MyBool`.

You may optionally specify:

* [`availability`](#availability)

## Renaming {#renaming}

You may `expose` or `offer` a capability by a different name:

```json5
{
    offer: [
        {
            config: "fuchsia.config.MyBool",
            from: "#child-a",
            to: [ "#child-b" ],
            as: "fuchsia.childB.SayHello",
        },
    ],
}
```

## Configuration types

There are many supported types for configuration values.

There are the following types for unsigned integers:

- `uint8`
- `uint16`
- `uint32`
- `uint64`

There are the following types for signed integers:

- `int8`
- `int16`
- `int32`
- `int64`

There is the `bool` type which supports values of `true` or `false`.

There is the `string` type, which must have `max_size` set in addition to the string value.

String example:

```json5
{
    capabilities: [
        {
            config: "fuchsia.config.MyString",
            type: "string",
            max_size: 100,
            value: "test",
        },
    ]
}
```

There is the `vector` type, which must have a `element` field with `type` also set. Vectors can
contain all other types except for vectors.

Vector example:

```json5
{
    capabilities: [
        {
            config: "fuchsia.config.MyUint8Vector",
            type: "vector",
            element: { type: "uint8" },
            max_count: 100,
            value: [1, 2, 3 ],
        },
        {
            config: "fuchsia.config.MyStringVector",
            type: "vector",
            element: {
                type: "string",
                max_size: 100,
            },
            max_count: 100,
            value: [
                "Hello",
                "World!",
            ],
        },
    ],
}
```

## Optional routing {#availability}

`Use`, `Offer`, and `Expose` for configuration capabilities support availability. This means that
a component can `optionally` use a configuration capability. For more information, please see
[consuming optional capabilities][consuming-optional-capabilities]

## Resolving routes and broken routes

Structured configuration values are handed to a component when it starts up.
This means that the Component Framework must resolve all of the routes for
configuration capabilities before the component is started.

A side effect of this means that the Component Framework is unable to start a
component that has a "broken route" (a route that does not successfully end in a
capability definition) for a configuration capability. For example, if you
request `fuchsai.config.MyBool` but your parent does not offer it to you, you
will not be able to start. This is different than other capabilities, where the
component will discover at runtime that it may not exist.

## Updating configuration values

Because configuration values are handed to a component when it starts, the component
will not see any updates if the capability route changes while the component is running.

In order to launch a component with a different configuration the following things need to
happen:

1) Update the CML providing the configuration capbility (likely by re-resolving it).
2) Stop and then Start the component that is using the configuration capbility.


[structured-configuration]: /docs/reference/components/structured_config.md
[capability-routing]: /docs/concepts/components/v2/capabilities/README.md#routing
[consuming-optional-capabilities]: /docs/development/components/connect.md#consuming-optional-capabilities
[docs-capability]: https://fuchsia.dev/reference/cml#capabilities