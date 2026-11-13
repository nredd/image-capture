# Native boundary contract

This contract is implemented by the macOS native adapter and shared portable driver. It is not independent audit approval or evidence of hardware validation.

- Only one native runtime may be created per process. The native owner stays on the main thread and never crosses a `Send` or `Sync` boundary. A bounded process-lifetime delegate proxy holds only a disconnectable ingress; browser and device delegate properties are unretained. Browser stop and session close are not assumed to drain callbacks.
- Open, command and close use distinct request-specific heap blocks. Each block captures only owned thread-safe ingress, immutable identity and once-consumption state. Install owner request state before submission, including reentrant completion. Delegates never settle a request. Completion capacity is separate from events; overflow is explicit.
- Open and close completion blocks may run on any available queue. The PTP callback queue is not specified. Copy bounded callback buffers into owned messages before returning. Keep command and outgoing native data alive through their required native last use; block capture lifetime alone does not establish input lifetime.
- A wait timeout is not cancellation. A late successful open must close after completion; a pending command must settle before close; a pending or failed close remains quarantined. Missing callbacks or failed close may indefinitely delay joined shutdown.
- Once work has settled and closure succeeded, disconnect ingress, discard queued messages, join owned execution, and release native references on their required thread. The scoped worker is joined even during Rust unwind; unexpected owner unwind retains the one unresolved native graph instead of freeing it. This does not assert that all foreign callbacks have ceased. Foreign-retained blocks can outlive the owner and only reach independent owned ingress.
- The design trusts Apple's asynchronous API to maintain valid ownership for every invocation, including concurrent final block release. The headers do not separately document the exact block retain duration. Foreign framework retention and framework-internal allocations are not bounded by this wrapper.
- The selected build SDK, toolchain, deployment target, dependency versions, every unsafe expression and implementation, ABI, nullability, selector, pointer/buffer lifetime, callback thread, delegate lifetime and input-retention invariant require an independent source-backed audit before hardware use.

References:
- [Apple ImageCaptureCore](https://developer.apple.com/documentation/imagecapturecore)
- [objc2 ImageCaptureCore bindings](https://docs.rs/objc2-image-capture-core/0.3.2/objc2_image_capture_core/)
- [block2 block ownership](https://docs.rs/block2/0.6.2/block2/struct.RcBlock.html)
- [Clang block ABI](https://clang.llvm.org/docs/Block-ABI-Apple.html)
