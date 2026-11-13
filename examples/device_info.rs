//! Compile-only by default. Explicit hardware admission is required to run ICC.
#![forbid(unsafe_code)]

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use image_capture::{
        client::{DeviceId, Error, Timeout},
        native,
    };
    use std::time::Duration;
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--hardware-admitted") {
        eprintln!("compile-only example; independent native audit and hardware admission required");
        return Ok(());
    }
    // The identifier stays local; never print camera identity or DeviceInfo data.
    let selected = DeviceId::new(args.next().ok_or(Error::InvalidInput)?)?;
    if args.next().is_some() {
        return Err(Error::InvalidInput.into());
    }
    native::run(move |mut client| -> Result<(), Error> {
        let timeout = Timeout::default();
        let devices = client.discover(Duration::from_secs(2), timeout)?;
        if devices.iter().filter(|id| **id == selected).count() != 1 {
            return Err(Error::NotFound);
        }
        for _ in 0..2 {
            let mut session = client.open(selected.clone(), timeout)?;
            // Only standard GetDeviceInfo. No settings, shutter or Sony handshake.
            let result = session.transact(0x1001, &[], vec![], timeout)?;
            let success = result.response.code == 0x2001 && !result.data.is_empty();
            session.close(timeout)?;
            if !success {
                return Err(Error::InvalidResponse);
            }
        }
        Ok(())
    })??;
    println!("device_info_cycles=2 confirmed_closes=2");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("native runtime requires macOS");
}
