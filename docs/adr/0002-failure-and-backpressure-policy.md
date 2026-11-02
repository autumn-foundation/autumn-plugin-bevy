# ADR 0002: Failure, back-pressure and HTTP errors

- Status: Accepted
- Date: 2026-11-02
- Tags: errors, security, reliability

## Context

The world runs user code: systems and commands. That code can panic, loop,
or write `AppExit`. Handlers turn the result into HTTP responses. Two facts
of autumn-web 0.9 matter here:

- `AutumnError` has `impl<E: Error> From<E>`. So `?` on any error gives a
  500, and a crate cannot write its own `From`.
- Autumn hides the detail of each 5xx response. In development it can show
  the error text.

## Decision

| Event | Result |
| --- | --- |
| The queue is full | The call fails at once with `Busy` (503). |
| A command panics | That call fails with `CommandPanicked` (500). The world continues. |
| A command takes too long | The call fails with `Timeout` (503). A command that did not start does not run. |
| An update panics | The status becomes `Failed`. The queue is rejected. The server continues. |
| A system writes `AppExit::Success` | The status becomes `Stopped`. |
| A system writes `AppExit::Error` | The status becomes `Failed`. |
| Setup panics, or the first update is late | The startup hook fails, so the server does not start. |
| Shutdown is late | `shutdown` returns `ShutdownTimeout`. The hook logs it. The thread stops later. |

HTTP errors:

1. The `Bevy` extractor methods return `AutumnResult`. They map each error
   with `RuntimeError::into_autumn_error`.
2. The status is 503 when a retry can help, and 500 for the other errors.
3. The body text is fixed: "bevy runtime is busy", "bevy runtime is
   unavailable" or "internal error". Panic text goes to the log only.
4. `BevyHandle` returns the typed `RuntimeError` for code that is not a
   handler.

## Why a command panic does not stop the world

A command is often one request. One bad request must not stop the game for
all users. The panic can leave the world in a partial state. The docs say
so. A later version can add an opt-in policy that stops the world on any
panic.

## Why an update panic stops the world

A panic in a system can leave a resource out of the world (Bevy takes a
resource out while a scope runs). The next update can then panic again in a
loop. A stop is safe and easy to see: health is down and calls get 503.
There is no restart: the app decides what to do.

## Consequences

- A handler must use the `Bevy` methods, not `?` on a `RuntimeError`. The
  README and the docs of `into_autumn_error` say this.
- `RuntimeError` is the name, not `BevyError`, because the Bevy prelude
  exports its own `BevyError`.
