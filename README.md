# icc-ptp-safe

Experimental Rust ImageCaptureCore boundary. The macOS adapter implements discovery, exact selection, owned session leases, serialized PTP, bounded advisory events, close and same-runtime reopen.

Established so far:

- An AI-assisted independent source review of the native code. The unsafe-site inventory is `docs/unsafe-audit.md`. This is NOT a certification or a formal audit
- ONE short real-camera sample at source `21df87991d833cd046fd5f9b74cd7451ab0aa257` (tree-identical to the release source): discovery found one candidate, then two open / GetDeviceInfo (operation `0x1001`, response `0x2001`) / confirmed-close cycles ran in one process, with a same-process reopen. The runtime joined and the Apple camera helpers were left untouched

Everything else is unverified, see the limits below. No production support claim follows.

## Use

Requires Rust 1.99+, macOS 13+ and Apple ImageCaptureCore. `native::run` must be called on the main thread; it drives the default-mode run loop and joins one Rust worker. The worker receives a non-cloneable, owned `Send` `client::Client`, not an Objective-C object. `client` and `native` are macOS-only APIs; framing, events, ingress and lifecycle models are portable.

The worker calls `discover(window, timeout)`, selects an exact `DeviceId`, and calls `open(id, timeout)` for an exclusive `Session`. `transact(operation, parameters, outgoing, timeout)` returns a parsed PTP response and separate data bytes. Response transaction IDs are opaque. Interpreting callback argument one as data and argument two as a PTP response is a per-device validation hypothesis, not a universal header-proven guarantee; the admitted read-only trial must validate it. Non-success PTP response codes remain visible to the caller. ICC owns session negotiation: raw PTP OpenSession/CloseSession is rejected. There is no Sony handshake, image parser, settings writer or shutter implementation here.

`Session::close` waits for confirmed closure; dropping a lease requests close without blocking the worker. After confirmed close, the same runtime can open another session. Only ONE native runtime may ever be constructed per process, including after failure. No native object is `Send` or `Sync`.

`examples/device_info.rs` is a real compiled consumer that discovers an explicitly selected camera, runs GetDeviceInfo, closes, reopens and repeats. Running it without arguments does NOT start ICC. Only the designated hardware owner, with explicit execution admission, may supply `--hardware-admitted` and an exact identifier. Do not put device identities or returned DeviceInfo bytes in shared logs. CI never passes that flag.

## Limits and cancellation

- One queued request, one admitted native request, one session graph, and 64 queued advisory events. Completion has its own reserved slot. Discovery returns at most 64 cameras; identifiers are at most 1024 UTF-8 bytes (the native UTF-16 precheck is conservatively 341 code units). Outgoing/incoming data is limited to 16 MiB each; responses to 32 bytes. Native copies and owned reply decoding can transiently duplicate these bounded buffers.
- Timeouts have separate admission and wait limits, each at most one hour. Discovery windows are at most 60 seconds. Expired admission submits nothing: the native backend rechecks a fresh monotonic clock after preparing inputs/blocks, immediately before ICC entry. Only a definite-not-submitted rejection withdraws registered state; it is not native cancellation. A dropped/expired caller close can still require a separate no-deadline cleanup close. Expired waits and receiver drops DO NOT cancel native work, free inputs, permit overlapping close, or authorize a replacement session. An abandoned successful open closes only after completion; an abandoned command settles before close. One small wait-interest token survives reply publication, so Timeout racing a queued reply cannot strand the lease. Post-receive admission rechecks that token before replacing it with the next serial request.
- Missing callbacks can block joined shutdown forever. Failed close or selected-device removal quarantines the single graph indefinitely. No detached owner, helper termination, cancellation claim, callback lifetime budget or accumulated replacement generations.
- Request IDs are monotonically checked for overflow. Their process-wide uniqueness supplies the generation boundary across close/reopen. Only request-specific blocks settle operations. Delegate notifications are uncorrelated advisory observations, never completion evidence.
- Event overflow is sticky via `events().take_overflow()`. An unexpected full reserved completion slot sets an independent sticky invariant fault; the owner reports `CallbackFailure`, quarantines and retains unresolved ownership rather than silently discarding completion. `Event.0` starts with a tag: 1/2 browser add/remove; 3/4 delegate open/close observations; 5 selected/previous-device removal; 6 native error observation; 7 capability change; 8 catalog ready; 9/10 access restriction removed/enabled; 11 raw PTP event bytes (at most 32 bytes after the tag); 12 oversized PTP event discarded. These events are NOT generation-correlated, and late old-device notifications can conservatively quarantine the runtime. No event completeness promise is made.
- The wrapper trusts Apple to maintain validity/ownership for asynchronous block invocations and valid callback-scoped native arguments. Immutable native inputs stay retained through completion, relying on ordinary API semantics that input last use has ended then OR ICC independently owns any later use. The headers do not prove a stronger last-use timing guarantee. Foreign-retained blocks capture only small thread-safe ingress, not native/session graphs. Joining owned execution does not claim final foreign-block disposal or global callback quiescence. Framework retention, enumeration and internal allocations are outside wrapper bounds.

## Development and admission

Run `make all` with stable Rust, rustfmt, clippy and cargo-deny 0.20.2. Set `CARGO_TARGET_DIR` for external build storage; builds default to two workers. The gate checks format, strict pedantic clippy, all targets, portable and macOS controlled callback tests, warning-free docs, licenses/advisories, the real compiled example, packaging, extracted tests and a separate extracted-package consumer. The example's default nonhardware path also runs. CI covers Linux portable modules and macOS controlled native code, never cameras.

`docs/native-contract.md` records ownership and trusted assumptions. `docs/unsafe-audit.md` maps every native unsafe site, selected SDK, pinned bindings and audit questions. Tests exercise production driver/block code, but neither tests nor coverage replace independent native safety audit. Hardware validation is a separate admission gate, not implied by `make all`. Releases are described in `docs/releasing.md`.

Signing/permissions, LaunchAgent execution, screen lock, physical reconnect, callback liveness and worker/owner behavior under real hardware failures remain unverified, as do timeouts, missing callbacks, disconnect and endurance on real hardware. No production support claim follows from a short sample or the earlier separate Swift experiment.

## License

MIT, see `LICENSE`. Apple frameworks are system dependencies, not redistributed code. Pinned Rust dependency licenses are checked by `cargo deny`; see the audit inventory for feature scope.
