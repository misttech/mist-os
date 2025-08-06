# Using rust-analyzer for Rust code modification

When you are tasked with writing or modifying Rust code (files with a
`.rs` extension), you must follow a strict "analyze-first"
methodology. Your primary goal is to prevent compilation errors
through proactive investigation of the existing codebase *before*
writing new code.

## The mandatory Rust workflow

You must follow these steps in order:

1.  **Analyze and Plan (MANDATORY):** Before writing or changing any
    code, you must investigate the APIs you intend to use.
    *   **Identify Key Types:** Identify the primary structs, enums,
        and traits relevant to the change.
    *   **Verify API Existence and Usage:** For each type, use the
        `hover` tool on existing variables of that type to see its
        documentation and available methods. This is the primary way
        to confirm a function exists and to learn its signature.
    *   **Understand Function Behavior:** Use the `definition` tool to
        read the source code of functions you plan to call. This is
        critical for understanding their behavior, parameters, and
        return types.
    *   **Formulate a Plan:** Based on your analysis, create a clear
        plan for your code changes. You should be confident that the
        functions and methods in your plan exist and that you
        understand how to use them correctly.

2.  **Implement and Test:**
    *   Write the code according to your plan, adhering to existing
        patterns and conventions.
    *   When adding new functionality, add a corresponding unit test
        to validate it.

3.  **Verify with Build:**
    *   After implementing the change and related tests, run the
        appropriate build command (e.g., `fx build`) to ensure your
        code compiles successfully.

4.  **Diagnose errors (if necessary):**
    *   The "Analyze and Plan" step should prevent most compilation
        errors. However, if the build fails, use the "Error Diagnosis
        Guide" below to understand and fix the issue. This guide is a
        fallback, not the primary workflow.

## Error diagnosis guide

If, after following the mandatory analysis workflow, you still
encounter an error, use these guides to diagnose the error.

### Unresolved import / "no function or associated item named" errors

1.  **Re-verify with `hover`:** Use the `hover` tool on the variable
    or type in question. The hover information is the source of truth
    for available methods. The function you are trying to call is
    likely named differently or does not exist on that type.
2.  **Find the Definition:** Use the `definition` tool on the type
    itself to navigate to its source and review its `impl` blocks to
    see all available functions.

### Trait bound errors (`the trait ... is not satisfied`)

1.  **Identify the Missing Trait:** The compiler error will state
    which trait is missing.
2.  **Locate the Type Definition:** Use the `definition` tool to find
    the struct/enum definition.
3.  **Add the Trait:** Add the required trait to the `#[derive(...)]`
    attribute.
4.  **Check Field Dependencies:** If adding a derive macro causes a
    new error, it means a field within the struct does not implement
    that trait. Recursively apply this process to the field's type.

### Borrow checker errors (`borrow of partially moved value`)

1.  **Identify the Move and Borrow:** The compiler will point to where
    a field was moved and where `self` was subsequently borrowed.
2.  **Reorder Operations:** Ensure that all method calls that borrow
    `self` (e.g., `self.validate()`) are performed *before* any fields
    are moved out of `self`.
