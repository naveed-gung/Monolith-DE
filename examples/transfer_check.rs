//! Offline child-process transfer checks. No network or media downloads.
use monolith_desktop::{library::Result, transfer};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).is_some_and(|v| v == "--ignore-config") {
        let out = PathBuf::from(&args[args.iter().position(|v| v == "-P").unwrap() + 1]);
        std::fs::write(out.join("unfinished.part"), b"partial")?;
        match args.last().unwrap().as_str() {
            "https://youtu.be/cancel" => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            "https://youtu.be/fail" => {
                eprintln!("fixture: deliberate failure");
                std::process::exit(2)
            }
            _ => {
                println!("PROGRESS:50%");
                std::fs::write(out.join("fixture.m4a"), b"fixture audio")?;
                return Ok(());
            }
        }
    }
    let dir = tempfile::tempdir()?;
    let exe = std::env::current_exe()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let mut updates = 0;
    let output = transfer::download(
        &exe,
        "https://youtu.be/success",
        dir.path(),
        false,
        None,
        cancel.clone(),
        |_, _| updates += 1,
    )?;
    assert_eq!(std::fs::read(&output)?, b"fixture audio");
    assert!(updates > 0);
    let collision = transfer::download(
        &exe,
        "https://youtu.be/success",
        dir.path(),
        false,
        None,
        cancel.clone(),
        |_, _| {},
    );
    assert!(collision.is_err());
    assert_eq!(std::fs::read(&output)?, b"fixture audio");
    let failed = transfer::download(
        &exe,
        "https://youtu.be/fail",
        dir.path(),
        false,
        None,
        cancel.clone(),
        |_, _| {},
    );
    assert!(
        failed
            .unwrap_err()
            .to_string()
            .contains("deliberate failure")
    );
    let signal = cancel.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        signal.store(true, Ordering::Relaxed);
    });
    let cancelled = transfer::download(
        &exe,
        "https://youtu.be/cancel",
        dir.path(),
        false,
        None,
        cancel,
        |_, _| {},
    );
    thread.join().unwrap();
    assert!(cancelled.unwrap_err().to_string().contains("cancelled"));
    assert_eq!(
        std::fs::read_dir(dir.path())?.count(),
        1,
        "Only the completed file should remain"
    );
    println!(
        "PASS: child success/progress, failure, cancellation, temporary cleanup, destination collision; no network."
    );
    Ok(())
}
