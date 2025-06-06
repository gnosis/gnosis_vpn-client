use std::sync::atomic::{AtomicBool, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(true);

fn main() -> Result<(), wintun_bindings::BoxError> {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace")).init();
    let mut dll_path = wintun_bindings::get_wintun_bin_pattern_path()?;
    if !std::fs::exists(&dll_path)? {
        dll_path = "wintun.dll".into();
    }
    let wintun = unsafe { wintun_bindings::load_from_path(dll_path)? };

    let version = wintun_bindings::get_running_driver_version(&wintun);
    log::info!("Using wintun version: {:?}", version);

    let adapter = match wintun_bindings::Adapter::open(&wintun, "Demo") {
        Ok(a) => a,
        Err(_) => wintun_bindings::Adapter::create(&wintun, "Demo", "Example", None)?,
    };

    let version = wintun_bindings::get_running_driver_version(&wintun)?;
    log::info!("Using wintun version: {:?}", version);

    let session = adapter.start_session(wintun_bindings::MAX_RING_CAPACITY)?;

    let reader_session = session.clone();
    let reader = std::thread::spawn(move || {
        while RUNNING.load(Ordering::Relaxed) {
            let packet = reader_session.receive_blocking();
            if let Err(err) = packet {
                log::info!("Error reading packet: {:?}", err);
                break;
            }
            let packet = packet?;
            let bytes = packet.bytes();
            let len = bytes.len();
            let data = &bytes[0..(20.min(bytes.len()))];
            println!("Read packet size {} bytes. Header data: {:?}", len, data);
        }
        Ok::<(), wintun_bindings::Error>(())
    });
    println!("Press enter to stop session");

    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    println!("Shutting down session");

    RUNNING.store(false, Ordering::Relaxed);
    session.shutdown()?;
    let _ = reader
        .join()
        .map_err(|err| wintun_bindings::Error::from(format!("{:?}", err)))?;

    println!("Shutdown complete");
    Ok(())
}
