# Coding standards

## Tests assert an observable value

A test must assert a value a caller can observe: a returned result, a rendered element, a state field, a message on the wire. Reviewers reject `let _ =` wrapped around an assertion-producing call inside a test, because discarding the result means the test cannot fail and proves nothing.

Reject:

```rust
let _ = cx.update(|cx| try_find_by_label("pane-title", cx));
```

Accept:

```rust
assert!(cx.update(|cx| try_find_by_label("pane-title", cx).is_some()));
```

This is a review rule. It is read while reviewing a diff, not while writing one.
