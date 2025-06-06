use futures::AsyncReadExt;
use std::sync::atomic::{AtomicBool, Ordering};
use wintun_bindings::{
    get_running_driver_version, get_wintun_bin_pattern_path, load_from_path, Adapter, AsyncSession, BoxError, Error,
    MAX_RING_CAPACITY,
};

static RUNNING: AtomicBool = AtomicBool::new(true);

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace")).init();
    let mut dll_path = get_wintun_bin_pattern_path()?;
    if !std::fs::exists(&dll_path)? {
        dll_path = "wintun.dll".into();
    }
    let wintun = unsafe { load_from_path(dll_path)? };

    let version = get_running_driver_version(&wintun);
    log::info!("Using wintun version: {:?}", version);

    let adapter = match Adapter::open(&wintun, "Demo") {
        Ok(a) => a,
        Err(_) => Adapter::create(&wintun, "Demo", "Example", None)?,
    };

    let version = get_running_driver_version(&wintun)?;
    log::info!("Using wintun version: {:?}", version);

    let session = adapter.start_session(MAX_RING_CAPACITY)?;

    let mut reader_session: AsyncSession = session.clone().into();
    let reader = tokio::task::spawn(async move {
        while RUNNING.load(Ordering::Relaxed) {
            let mut bytes = [0u8; 1500];
            let len = reader_session.read(&mut bytes).await?;
            if len == 0 {
                println!("Reader session closed");
                break;
            }
            let data = &bytes[0..(20.min(bytes.len()))];
            println!("Read packet size {} bytes. Header data: {:?}", len, data);
        }
        Ok::<(), Error>(())
    });
    println!("Press enter to stop session");

    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    println!("Shutting down session");

    RUNNING.store(false, Ordering::Relaxed);
    session.shutdown()?;
    let _ = reader.await?;

    println!("Shutdown complete");
    Ok(())
}
