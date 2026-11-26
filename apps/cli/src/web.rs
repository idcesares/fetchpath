//! `fetchpath web`: the loopback web UI of the engine (FP-104).

use crate::client::{self, Engine};
use crate::download::EXIT_USAGE;
use crate::queue;
use fetchpath_protocol::ProtocolError;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::error::{ErrorCode, ErrorScope};
use fetchpath_protocol::message::CommandResult;

const USAGE: &str = "usage: fetchpath web on | off | open | sign-out";

pub fn run(args: &[String]) -> i32 {
    let action = match args {
        [word] if matches!(word.as_str(), "on" | "off" | "open" | "sign-out") => word.as_str(),
        _ => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    match Engine::connect().and_then(|engine| act(&engine, action)) {
        Ok(()) => 0,
        Err(error) => client::fail(&error, false),
    }
}

fn act(engine: &Engine, action: &str) -> Result<(), ProtocolError> {
    println!("{}", act_message(engine, action)?);
    Ok(())
}

/// Runs one `/web` action against the engine and returns the human message.
pub(crate) fn act_message(engine: &Engine, action: &str) -> Result<String, ProtocolError> {
    match action {
        "on" | "off" => {
            let on = action == "on";
            queue::settings_outcome(engine, &["web_ui".into(), on.to_string()])?;
            Ok(if on {
                "The web UI is on. Run `fetchpath web open` (or /web open) to sign in this browser."
            } else {
                "The web UI is off and every browser is signed out."
            }
            .into())
        }
        "open" => match engine.send(Command::OpenWebUi)? {
            // The link carries a single-use ticket: it goes to the browser,
            // never to the screen.
            CommandResult::WebUiLink { url } => {
                open_in_browser(url.expose()).map_err(|message| {
                    ProtocolError::new(ErrorCode::INTERNAL_UNKNOWN, ErrorScope::Command, message)
                })?;
                Ok("Opening Fetchpath in your browser.".into())
            }
            other => Err(client::unexpected(&other)),
        },
        "sign-out" => match engine.send(Command::SignOutBrowsers)? {
            CommandResult::BrowsersSignedOut => Ok("Every browser is signed out.".into()),
            other => Err(client::unexpected(&other)),
        },
        _ => Err(client::input_error("Say on, off, open, or sign-out.")),
    }
}

fn open_in_browser(url: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the string is NUL-terminated and outlives the call; the other
    // arguments are null, which ShellExecuteW accepts.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result as isize) <= 32 {
        return Err("Could not open the web UI in your browser.".into());
    }
    Ok(())
}
