# testbed

`testbed` is a suite of integration tests for component bedrock, exercising
launching programs and communication between them.

## Building

To add this component to your build, append
`--with src/sys/bedrock/testbed`
to the `fx set` invocation.

## Testing

Run the tests for testbed using the `bedrock-testbed` package.

```
$ fx test bedrock-testbed
```
