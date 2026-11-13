//! External extracted-package consumer. Never starts discovery or a camera.
#![forbid(unsafe_code)]
#[cfg(target_os = "macos")]
use icc_ptp_safe::client::{Client, DeviceId, Error};
use icc_ptp_safe::framing;

#[cfg(target_os = "macos")]
fn owned_send<T: Send>() {}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(framing::command(0x1001, &[])?.len(), 12);
    assert!(framing::response(&[0; 12]).is_err());
    #[cfg(target_os = "macos")]
    {
        owned_send::<Client>();
        assert_eq!(DeviceId::new(String::new()), Err(Error::InvalidInput));
        // Monomorphize/link the actual adapter without invoking native entry.
        let entry = icc_ptp_safe::native::run::<fn(Client), ()>;
        std::hint::black_box(entry);
    }
    println!("extracted_consumer=pass native_hardware=not_run");
    Ok(())
}
