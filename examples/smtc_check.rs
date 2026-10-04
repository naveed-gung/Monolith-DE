//! Reports what the Windows system media transport controls know about the
//! running player. Read-only probe: it enumerates the global sessions, then
//! asks the player's own session to toggle play/pause so the app's
//! `ButtonPressed` handler is exercised through Windows rather than through a
//! synthetic key press. Start the player first, begin playback, then run this.

#[cfg(not(windows))]
fn main() {
    eprintln!("SKIP: Windows-only probe.");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
    };

    let manager = Manager::RequestAsync()?.get()?;
    let sessions = manager.GetSessions()?;
    println!("sessions: {}", sessions.Size()?);

    let describe = |session: &Session| -> windows::core::Result<String> {
        let info = session.GetPlaybackInfo()?;
        let controls = info.Controls()?;
        Ok(format!(
            "id={} status={:?} play={} pause={} next={} previous={}",
            session.SourceAppUserModelId()?,
            info.PlaybackStatus()?.0,
            controls.IsPlayEnabled()?,
            controls.IsPauseEnabled()?,
            controls.IsNextEnabled()?,
            controls.IsPreviousEnabled()?,
        ))
    };

    let mut player = None;
    for session in &sessions {
        let line = describe(&session)?;
        println!("  {line}");
        if line.to_lowercase().contains("monolith") {
            player = Some(session);
        }
    }

    match manager.GetCurrentSession() {
        Ok(current) => println!("current: {}", describe(&current)?),
        Err(_) => println!("current: <none>"),
    }

    let Some(player) = player else {
        println!("FAIL: the player registered no media session.");
        return Ok(());
    };

    let before = player.GetPlaybackInfo()?.PlaybackStatus()?.0;
    let accepted = player.TryTogglePlayPauseAsync()?.get()?;
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let after = player.GetPlaybackInfo()?.PlaybackStatus()?.0;
    println!("toggle accepted={accepted} status {before:?} -> {after:?}");
    if accepted && before != after {
        println!("PASS: Windows delivered the transport command to the player.");
    } else {
        println!("FAIL: the player did not react to the transport command.");
    }
    Ok(())
}
