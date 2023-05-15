## multibind-composite-test

This test verifies that multibind legacy and spec composites work as expected.

It adds following nodes to parent the composites:

* Node A
* Node B
* Node C
* Node C

Since we are testing for multibind, the composites will share parents.
The composite breakdown is as follows:

**Legacy composite 1**

* Node A (primary)
* Node B

**Legacy composite 2**

* Node B (primary)
* Node C

**Composite node spec 1**

* Node A (primary)
* Node C
* Node D

**Composite node spec 2**

* Node B
* Node D (primary)
