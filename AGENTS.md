# Repository guidance

This repository owns a narrowly scoped native FFI boundary that has had an AI-assisted independent source review (not a certification or formal audit). Unsafe code is authorized only for internal, documented ImageCaptureCore interoperability with source-backed invariants and an audit entry for every unsafe expression and implementation. Portable protocol and state modules forbid unsafe. Deny unsafe operations in unsafe functions; never suppress safety lints. No unsafe Send or Sync implementations for native objects. This approval does not change tethercam's unsafe prohibition.

Keep native state on its required thread and retain it until callbacks settle. A deadline does not cancel native work. No camera hardware execution without explicit owner admission. The repository is public and the crate is published to crates.io only through `docs/releasing.md`: the owner publishes the first version by hand, later versions go through the tag-triggered OIDC workflow. Never create tags, releases, environments or rulesets, or publish, without explicit authorization.

Run the full repository gate before committing. Keep docs and tests current. No secrets, private device identities or absolute local paths in repository artifacts.
