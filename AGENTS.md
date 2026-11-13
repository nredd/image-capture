# Repository guidance

This repository owns a narrowly scoped, independently audited native FFI boundary. Unsafe code is authorized only for private, documented ImageCaptureCore interoperability with source-backed invariants and an audit entry for every unsafe expression and implementation. Portable protocol and state modules forbid unsafe. Deny unsafe operations in unsafe functions; never suppress safety lints. No unsafe Send or Sync implementations for native objects. This approval does not change tethercam's unsafe prohibition.

Keep native state on its required thread and retain it until callbacks settle. A deadline does not cancel native work. No camera hardware execution before independent native pre-audit admission. No release, registry publication or visibility change without explicit authorization.

Run the full repository gate before committing. Keep docs and tests current. No secrets, private device identities or absolute local paths in repository artifacts.
