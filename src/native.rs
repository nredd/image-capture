//! Main-thread ICC runtime. No hardware use before independent native audit.
//!
//! [`run`] owns a main-thread run loop and joins one portable Rust worker.
//! Missing callbacks or quarantined closure can keep it blocked indefinitely.
use crate::{
    client::{self, Client, DeviceId, Error, MAX_DEVICES, MAX_ID_BYTES},
    driver::{Backend, Driver},
    events::{Event, EventSender, event_channel},
    ingress::{CompletionSender, completion_channel},
    lifecycle::RequestId,
    native_callbacks::{PtpCompletion, ptp_block, session_block},
};
use block2::RcBlock;
use objc2::{
    AnyThread, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::ProtocolObject,
};
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSData, NSDate, NSDefaultRunLoopMode, NSDictionary, NSError,
    NSObject, NSObjectProtocol, NSRunLoop,
};
use objc2_image_capture_core::{
    ICCameraDevice, ICCameraDeviceCanAcceptPTPCommands, ICCameraDeviceDelegate, ICCameraItem,
    ICDevice, ICDeviceBrowser, ICDeviceBrowserDelegate, ICDeviceDelegate, ICDeviceLocationTypeMask,
    ICDeviceTypeMask,
};
use std::{
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

static CREATED: AtomicBool = AtomicBool::new(false);
static DELEGATE_INGRESS: OnceLock<Arc<DelegateIngress>> = OnceLock::new();

/// Only owned Send state is reachable from the immortal delegate.
struct DelegateIngress {
    events: Mutex<Option<EventSender>>,
    removed: AtomicBool,
}
impl DelegateIngress {
    fn event(&self, bytes: Vec<u8>) {
        if let Ok(events) = self.events.lock()
            && let Some(sender) = &*events
        {
            let _ = sender.publish(Event(bytes));
        }
    }
    fn disconnect(&self) {
        if let Ok(mut events) = self.events.lock() {
            *events = None;
        }
    }
}
fn advisory(tag: u8) {
    if let Some(ingress) = DELEGATE_INGRESS.get() {
        ingress.event(vec![tag]);
    }
}

define_class!(
    // SAFETY: NSObject permits subclassing. This stateless class has no native
    // owner references or thread-affine ivars; callbacks use synchronized ingress.
    #[unsafe(super = NSObject)]
    struct DelegateProxy;
    // SAFETY: NSObjectProtocol adds no requirements beyond NSObject.
    unsafe impl NSObjectProtocol for DelegateProxy {}
    // SAFETY: All method ABIs below match the pinned generated browser protocol.
    // Borrowed object arguments are ignored, not retained or sent between threads.
    unsafe impl ICDeviceBrowserDelegate for DelegateProxy {
        #[unsafe(method(deviceBrowser:didAddDevice:moreComing:))]
        fn added(&self, _browser: &ICDeviceBrowser, _device: &ICDevice, _more: bool) {
            advisory(1);
        }
        #[unsafe(method(deviceBrowser:didRemoveDevice:moreGoing:))]
        fn removed(&self, _browser: &ICDeviceBrowser, _device: &ICDevice, _more: bool) {
            advisory(2);
        }
    }
    // SAFETY: Signatures match ICDeviceDelegate. Delegate observations NEVER
    // fulfill a request-specific completion. The proxy itself lives forever.
    unsafe impl ICDeviceDelegate for DelegateProxy {
        #[unsafe(method(device:didOpenSessionWithError:))]
        fn opened(&self, _device: &ICDevice, _error: Option<&NSError>) {
            advisory(3);
        }
        #[unsafe(method(device:didCloseSessionWithError:))]
        fn closed(&self, _device: &ICDevice, _error: Option<&NSError>) {
            advisory(4);
        }
        #[unsafe(method(didRemoveDevice:))]
        fn device_removed(&self, _device: &ICDevice) {
            if let Some(ingress) = DELEGATE_INGRESS.get() {
                ingress.removed.store(true, Ordering::Release);
            }
            advisory(5);
        }
        #[unsafe(method(device:didEncounterError:))]
        fn error(&self, _device: &ICDevice, _error: Option<&NSError>) {
            advisory(6);
        }
    }
    // SAFETY: Every required camera delegate selector is implemented with the
    // pinned generated ABI, including ignored catalog/thumbnail notifications.
    // Callback NSData is borrowed only during a bounded owned copy, under ICC's
    // valid and unmodified callback-buffer contract. No native value escapes.
    unsafe impl ICCameraDeviceDelegate for DelegateProxy {
        #[unsafe(method(cameraDevice:didAddItems:))]
        fn items_added(&self, _camera: &ICCameraDevice, _items: &NSArray<ICCameraItem>) {}
        #[unsafe(method(cameraDevice:didRemoveItems:))]
        fn items_removed(&self, _camera: &ICCameraDevice, _items: &NSArray<ICCameraItem>) {}
        #[unsafe(method(cameraDevice:didRenameItems:))]
        fn items_renamed(&self, _camera: &ICCameraDevice, _items: &NSArray<ICCameraItem>) {}
        #[unsafe(method(cameraDevice:didReceiveMetadata:forItem:error:))]
        fn metadata(
            &self,
            _camera: &ICCameraDevice,
            _metadata: Option<&NSDictionary>,
            _item: &ICCameraItem,
            _error: Option<&NSError>,
        ) {
        }
        #[unsafe(method(cameraDevice:didReceiveThumbnail:forItem:error:))]
        fn thumbnail(
            &self,
            _camera: &ICCameraDevice,
            _thumbnail: Option<&CGImage>,
            _item: &ICCameraItem,
            _error: Option<&NSError>,
        ) {
        }
        #[unsafe(method(cameraDeviceDidChangeCapability:))]
        fn capability(&self, _camera: &ICCameraDevice) {
            advisory(7);
        }
        #[unsafe(method(deviceDidBecomeReadyWithCompleteContentCatalog:))]
        fn catalog(&self, _camera: &ICCameraDevice) {
            advisory(8);
        }
        #[unsafe(method(cameraDeviceDidRemoveAccessRestriction:))]
        fn accessible(&self, _device: &ICDevice) {
            advisory(9);
        }
        #[unsafe(method(cameraDeviceDidEnableAccessRestriction:))]
        fn restricted(&self, _device: &ICDevice) {
            advisory(10);
        }
        #[unsafe(method(cameraDevice:didReceivePTPEvent:))]
        fn ptp_event(&self, _camera: &ICCameraDevice, data: &NSData) {
            // Never unwind a Rust panic through an Objective-C callback.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                if let Some(ingress) = DELEGATE_INGRESS.get() {
                    // Tag 11 denotes raw advisory PTP bytes, tag 12 oversize.
                    if data.len() > 32 {
                        ingress.event(vec![12]);
                        return;
                    }
                    let mut bytes = vec![11];
                    bytes.extend_from_slice(&data.to_vec());
                    ingress.event(bytes);
                }
            }));
        }
    }
);

/// Retain blocks and native input buffers until request completion, not timeout.
struct NativePending {
    _session: Option<RcBlock<dyn Fn(*mut NSError)>>,
    _ptp: Option<RcBlock<PtpCompletion>>,
    _command: Option<Retained<NSData>>,
    _outgoing: Option<Retained<NSData>>,
}
struct NativeOwner {
    browser: Retained<ICDeviceBrowser>,
    proxy: &'static DelegateProxy,
    ingress: Arc<DelegateIngress>,
    camera: Option<Retained<ICCameraDevice>>,
    pending: Option<NativePending>,
    _main_thread: PhantomData<Rc<()>>,
}
impl NativeOwner {
    fn new(ingress: Arc<DelegateIngress>) -> Self {
        // SAFETY: NSObject init is valid for this stateless NSObject subclass.
        let proxy: Retained<DelegateProxy> = unsafe { msg_send![DelegateProxy::alloc(), init] };
        // Both delegate properties are assign/unretained. There is no callback
        // barrier: leak ONE bounded proxy for this process's ONE native runtime.
        let proxy = &**Box::leak(Box::new(proxy));
        // SAFETY: Called only after run's main-thread check, no constructor args.
        let browser = unsafe { ICDeviceBrowser::new() };
        // SAFETY: Proxy conforms to the protocol and is process-lifetime retained.
        // Apple documents camera and local bitmasks as combinable. All owner
        // native access is main-thread confined; no owner reference is captured.
        unsafe {
            browser.setDelegate(Some(ProtocolObject::from_ref(proxy)));
            browser.setBrowsedDeviceTypeMask(ICDeviceTypeMask(
                ICDeviceTypeMask::Camera.0 | ICDeviceLocationTypeMask::Local.0,
            ));
            browser.start();
        }
        Self {
            browser,
            proxy,
            ingress,
            camera: None,
            pending: None,
            _main_thread: PhantomData,
        }
    }
    fn cameras(&self) -> Result<Vec<(DeviceId, Retained<ICCameraDevice>)>, Error> {
        // SAFETY: Browser is owned and accessed only on main. The binding retains
        // the returned array. Entries are checked via safe dynamic downcast.
        let devices = unsafe { self.browser.devices() };
        let Some(devices) = devices else {
            return Ok(vec![]);
        };
        if devices.len() > MAX_DEVICES {
            return Err(Error::LimitExceeded);
        }
        let mut cameras = Vec::new();
        for index in 0..devices.len() {
            let device = devices.objectAtIndex(index);
            if let Ok(camera) = device.downcast::<ICCameraDevice>() {
                // SAFETY: Live ICC object on its owner thread; returned string
                // is retained by the binding and bounded before UTF-8 copying.
                if let Some(id) = unsafe { camera.UUIDString() } {
                    // UTF-16 length bounds UTF-8 by at most 3 bytes per code unit.
                    if id.len() > MAX_ID_BYTES / 3 {
                        return Err(Error::LimitExceeded);
                    }
                    let id = DeviceId::new(id.to_string()).map_err(|_| Error::LimitExceeded)?;
                    cameras.push((id, camera));
                }
            }
        }
        Ok(cameras)
    }
    fn finish(&mut self) {
        self.ingress.disconnect();
        // SAFETY: Main-thread owned browser, no native operation remains. Stop is
        // NOT a delegate barrier; the proxy and disconnected ingress stay alive.
        unsafe {
            self.browser.stop();
        }
    }
}
impl Drop for NativeOwner {
    fn drop(&mut self) {
        self.ingress.disconnect();
        if self.camera.is_some() || self.pending.is_some() {
            // An unexpected Rust unwind must not free callback-reachable native
            // inputs or an unclosed camera. Keep the ONE graph, never replace it.
            std::mem::forget(self.pending.take());
            std::mem::forget(self.camera.take());
            std::mem::forget(self.browser.clone());
        }
    }
}
impl Backend for NativeOwner {
    fn devices(&self) -> Result<Vec<DeviceId>, Error> {
        Ok(self.cameras()?.into_iter().map(|(id, _)| id).collect())
    }
    fn select(&mut self, id: &DeviceId) -> Result<(), Error> {
        let mut matches = self
            .cameras()?
            .into_iter()
            .filter(|(candidate, _)| candidate == id);
        let (_, camera) = matches.next().ok_or(Error::NotFound)?;
        if matches.next().is_some() {
            return Err(Error::Ambiguous);
        }
        // SAFETY: This process-lifetime proxy conforms to ICDeviceDelegate and
        // ICCameraDeviceDelegate. Assignment is on the native owner thread.
        unsafe {
            camera.setDelegate(Some(ProtocolObject::from_ref(self.proxy)));
        }
        self.camera = Some(camera);
        Ok(())
    }
    fn open(&mut self, id: RequestId, sender: &CompletionSender) {
        let block = session_block(sender.request(id));
        self.pending = Some(NativePending {
            _session: Some(block.clone()),
            _ptp: None,
            _command: None,
            _outgoing: None,
        });
        if let Some(camera) = &self.camera {
            // SAFETY: Owner registered Opening and retained the heap block before
            // submission. Null options need no dictionary generic invariants.
            // ICC is trusted to keep async block invocations valid on any queue.
            unsafe {
                camera.requestOpenSessionWithOptions_completion(None, &block);
            }
        }
    }
    fn validate_command(&self) -> Result<(), Error> {
        let camera = self.camera.as_ref().ok_or(Error::Busy)?;
        // SAFETY: Retained camera on the owner thread; ICC declares the array
        // element type NSString. The exported capability constant is immutable.
        let (capabilities, required) =
            unsafe { (camera.capabilities(), ICCameraDeviceCanAcceptPTPCommands) };
        if capabilities.len() > 64 {
            return Err(Error::LimitExceeded);
        }
        if !capabilities.containsObject(required) {
            return Err(Error::NativeFailure);
        }
        Ok(())
    }
    fn command(
        &mut self,
        id: RequestId,
        sender: &CompletionSender,
        command: &[u8],
        outgoing: &[u8],
    ) {
        let command = NSData::with_bytes(command);
        let outgoing = (!outgoing.is_empty()).then(|| NSData::with_bytes(outgoing));
        // SAFETY: This block is passed only to ICC's declared completion API,
        // which supplies callback-scoped valid immutable NSData arguments.
        let block = unsafe { ptp_block(sender.request(id)) };
        self.pending = Some(NativePending {
            _session: None,
            _ptp: Some(block.clone()),
            _command: Some(command.clone()),
            _outgoing: outgoing.clone(),
        });
        if let Some(camera) = &self.camera {
            // SAFETY: Open session, one registered request, main-thread access.
            // Native inputs are immutable copies retained through completion.
            // ICC owns async block validity; captures contain only Send ingress.
            unsafe {
                camera.requestSendPTPCommand_outData_completion(
                    &command,
                    outgoing.as_deref(),
                    &block,
                );
            }
        }
    }
    fn close(&mut self, id: RequestId, sender: &CompletionSender) {
        let block = session_block(sender.request(id));
        self.pending = Some(NativePending {
            _session: Some(block.clone()),
            _ptp: None,
            _command: None,
            _outgoing: None,
        });
        if let Some(camera) = &self.camera {
            // SAFETY: Closing is installed before submission, prior request has
            // settled, and native owner/block retention matches open above.
            unsafe {
                camera.requestCloseSessionWithOptions_completion(None, &block);
            }
        }
    }
    fn settled(&mut self) {
        self.pending = None;
    }
    fn release_camera(&mut self) {
        self.camera = None;
    }
    fn disconnected(&self) -> bool {
        self.ingress.removed.load(Ordering::Acquire)
    }
}

/// Run ICC on the calling main thread and join a single owned Rust worker.
///
/// The closure receives only an owned `Send` client. It can discover, select,
/// open, transact, close and reopen. Returning or panicking stops admission and
/// requests orderly close. This function can block FOREVER on missing callbacks
/// or quarantined closure; it never detaches a worker or fabricates cancellation.
/// One native runtime EVER per process is permitted. Do not nest in another
/// UI runtime: this function drives the main default-mode run loop itself.
///
/// # Errors
/// Returns off-main, already-created, worker-start or joined-worker-panic errors.
/// Native request errors are returned by the worker's client operations.
pub fn run<F, T>(worker: F) -> Result<T, Error>
where
    F: FnOnce(Client) -> T + Send + 'static,
    T: Send + 'static,
{
    let _marker = MainThreadMarker::new().ok_or(Error::NotMainThread)?;
    CREATED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| Error::AlreadyCreated)?;
    let (events, received_events) = event_channel(64, 33);
    let ingress = Arc::new(DelegateIngress {
        events: Mutex::new(Some(events)),
        removed: AtomicBool::new(false),
    });
    let _ = DELEGATE_INGRESS.set(Arc::clone(&ingress));
    let (client, requests) = client::channel(received_events);
    // Start the owned worker before constructing native objects, so OS spawn
    // failure cannot strand an active native graph. Nothing native enters it.
    thread::scope(|scope| {
        let worker = thread::Builder::new()
            .name("icc-owned-worker".into())
            .spawn_scoped(scope, move || worker(client))
            .map_err(|_| {
                ingress.disconnect();
                Error::WorkerStartFailed
            })?;
        let (sender, completions) = completion_channel();
        let mut driver = Driver::new(NativeOwner::new(ingress), requests, completions, sender);
        let run_loop = NSRunLoop::mainRunLoop();
        loop {
            if worker.is_finished() {
                driver.stop();
            }
            autoreleasepool(|_| {
                driver.tick(Instant::now());
                let until = NSDate::dateWithTimeIntervalSinceNow(0.01);
                // SAFETY: Foundation exports this immutable run-loop mode constant;
                // this is the main run loop and every native access stays on main.
                let mode = unsafe { NSDefaultRunLoopMode };
                run_loop.runMode_beforeDate(mode, &until);
            });
            if driver.finished() {
                break;
            }
            // An empty run loop may return immediately. Avoid spinning while keeping
            // callbacks/requests responsive without a cross-thread native wakeup.
            thread::sleep(Duration::from_millis(1));
        }
        driver.backend.finish();
        drop(driver);
        worker.join().map_err(|_| Error::WorkerPanicked)
    })
}
