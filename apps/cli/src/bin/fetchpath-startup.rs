//! Windowless entry point for the opt-in Windows sign-in start (FP-108).
//! Keep the interactive executable a console application; reuse the normal
//! engine launcher so locking, authentication and update holds stay identical.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    use fetchpath_protocol::launch::{EngineHome, LAUNCH_WAIT, attach_or_launch};
    use fetchpath_protocol::pipe::Limits;

    let result = (|| {
        let home = EngineHome::from_env().map_err(|_| ())?;
        let exe = std::env::current_exe().map_err(|_| ())?;
        let engine = exe.with_file_name(if cfg!(windows) {
            "fetchpath.exe"
        } else {
            "fetchpath"
        });
        attach_or_launch(&home, &engine, Limits::default(), LAUNCH_WAIT).map_err(|_| ())?;
        Ok::<_, ()>(())
    })();
    if result.is_err() {
        std::process::exit(1);
    }
}
