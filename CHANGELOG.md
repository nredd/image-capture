# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0]

Initial public release. Experimental; no production support claim.

### Added

- Owned ImageCaptureCore session boundary for macOS: discovery, exact device selection, owned session leases, serialized PTP transactions, bounded advisory events, confirmed close and same-runtime reopen
- Portable framing, events, ingress and lifecycle models that build on any platform
- `device_info` example, which runs only its nonhardware path unless the hardware owner passes `--hardware-admitted`
- Native contract and unsafe audit documentation

### Verification

- AI-assisted independent source review of the native code. This is not a certification
- One short real-camera sample at source `21df87991d833cd046fd5f9b74cd7451ab0aa257` (tree-identical to main `69d36260c52719d8b6764aeb3fbe36a6858df3dc`; `src/` is byte-identical since then, `tests/`, `examples/` and `Cargo.lock` differ only by the crate rename, while README, manifest metadata, docs and CI also differ): discovery found one candidate, two open / GetDeviceInfo / confirmed-close cycles ran in one process, followed by a same-process reopen. The runtime joined and the Apple camera helpers were untouched
- Timeouts on real hardware, missing callbacks, disconnect, LaunchAgent execution, screen lock, endurance, and signing/permissions remain unverified
