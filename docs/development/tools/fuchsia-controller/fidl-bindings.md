# FIDL bindings in Python

This page provides examples of different FIDL types and how they are
instantiated and used in Python.

Most of the examples focus on how types are translated from FIDL to Python.
If you would like to learn about implementng FIDL servers or handling
async code, check out the [tutorial][getting-started] or the
[async Python best practices][best-practices] page.

## Bits and Enum types

Both bits and enum types are represented the same in Python, so this example
works for both.

* {FIDL}

  ```fidl {:.devsite-disable-click-to-copy}
  library fidl.example.library;

  type ExampleBits = strict bits {
      EXAMPLE_ONE = 0x01;
      EXAMPLE_TWO = 0x02;
  };
  ```

* {Python}

  ```py {:.devsite-disable-click-to-copy}
  import fidl.example_library

  x = fidl.example_library.ExampleBits.EXAMPLE_ONE
  y = fidl.example_library.ExampleBits.EXAMPLE_TWO
  z = fidl.example_library.ExampleBits(1)  # creates EXAMPLE_ONE
  a = fidl.example_library.ExampleBits(9999)
  print(x)
  print(y)
  print(z)
  print(a)
  ```

This Python code above prints the following output:

```sh {:.devsite-disable-click-to-copy}
<ExampleBits.EXAMPLE_ONE: 1>
<ExampleBits.EXAMPLE_TWO: 2>
<ExampleBits.EXAMPLE_ONE: 1>
<ExampleBits.EXAMPLE_ONE|EXAMPLE_TWO|9996: 9999>
```

## Struct types

In Python, types and size limits are not enforced during construction, so if you
were to make a struct with an overly long string, for example, the example below
would not create an error for the user until attempting to encode the struct to
bytes. This goes for other composited structures as well, such as tables and
unions.

* {FIDL}

  ```fidl {:.devsite-disable-click-to-copy}
  library example.library;

  type ExampleStruct = struct {
      field_one uint32;
      field_two string:256;
  };
  ```

* {Python}

  ```py {:.devsite-disable-click-to-copy}
  import example_library;

  x = fidl.example_library.ExampleStruct(field_one=123, field_two="hello")
  print(x)
  ```

This Python code above prints the following output:

```sh {:.devsite-disable-click-to-copy}
ExampleStruct(field_one=123, field_two="hello")
```

If you were to attempt to construct this type without one of the fields, it
would raise a TypeError exception listing which arguments were missing.

## Table types

These are identical to struct types but without the strict constructor
requirements. Any fields not supplied to the constructor will be set to `None`.

* {FIDL}

  ```fidl {:.devsite-disable-click-to-copy}
  library example.library;

  type ExampleTable = table {
      1: field_one uint32;
      2: field_two string:256;
      3: field_three vector<uint32>:256;
  };
  ```

* {Python}

  ```py {:.devsite-disable-click-to-copy}
  import fidl.example_library

  x = fidl.example_library.ExampleTable(field_one=2)
  y = fidl.example_library.ExampleTable(field_three=[1, 2, 3])
  z = fidl.example_library.ExampleTable()
  print(x)
  print(y)
  print(z)
  ```

This Python code prints the following output:

```sh {:.devsite-disable-click-to-copy}
ExampleTable(field_one=2, field_two=None, field_three=None)
ExampleTable(field_one=None, field_two=None, field_three=[1, 2, 3])
ExampleTable(field_one=None, field_two=None, field_three=None)
```

## Union types

In Python, there are multiple ways to construct this object.

* {FIDL}

  ```fidl {:.devsite-disable-click-to-copy}
  library example.library;

  type ExampleUnion = strict union {
      1: first_kind uint32;
      2: second_kind string:256;
  };
  ```

* {Python}

  ```py {:.devsite-disable-click-to-copy}
  import fidl.example_library

  x = fidl.example_library.ExampleUnion.first_kind_variant(123)
  y = fidl.example_library.ExampleUnion()
  y.second_kind = "hello"
  print(x)
  print(y)
  ```

This Python code prints the following output:

```sh {:.devsite-disable-click-to-copy}
<'example.library/ExampleUnion' object(first_kind=123)>
<'example.library/ExampleUnion' object(second_kind="hello")>
```

<!-- Reference links -->

[best-practices]: /docs/development/tools/fuchsia-controller/async-python.md
[getting-started]: /docs/development/tools/fuchsia-controller/getting-started-in-tree.md
