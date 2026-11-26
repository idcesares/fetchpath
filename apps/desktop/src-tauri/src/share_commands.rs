//! Public share links, relay enrollment, and QR image export commands (FP-117, FP-118, FP-127).

use crate::{Engine, off_thread, text, unexpected};
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model;
use fetchpath_protocol::qr_image;
use fetchpath_protocol::send::OfferSource;
use std::sync::Arc;

fn sharing_view(result: CommandResult) -> Result<model::ShareView, String> {
    match result {
        CommandResult::Sharing { sharing } => Ok(sharing),
        other => Err(unexpected(&other)),
    }
}

/// A link just made, with its QR code. The only time the link exists outside
/// the engine's memory: the engine cannot show it again.
#[derive(serde::Serialize)]
pub(crate) struct NewShare {
    link: String,
    share: model::ShareItem,
    /// The QR code's modules, row by row: `1` dark, `0` light.
    qr_rows: Vec<String>,
}

/// The modules of the link's QR code, row by row: `1` dark, `0` light.
fn qr_rows(link: &str) -> Option<Vec<String>> {
    let code =
        qrcode::QrCode::with_error_correction_level(link.as_bytes(), qrcode::EcLevel::L).ok()?;
    let width = code.width();
    Some(
        code.to_colors()
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|color| {
                        if *color == qrcode::Color::Dark {
                            '1'
                        } else {
                            '0'
                        }
                    })
                    .collect()
            })
            .collect(),
    )
}

/// Saves the QR code of a link just made as an image, to the file the person
/// chose in the save dialog. The image IS the link: this is the one way a link
/// reaches disk, and only on their explicit choice (SECURITY.md). Nothing is
/// logged, and the file type comes from the name (`.svg` or `.png`).
#[tauri::command]
pub(crate) async fn save_share_qr(path: String, link: String) -> Result<(), String> {
    off_thread(move || {
        let format = qr_image::Format::from_path(&path)
            .ok_or_else(|| "Save the image as a .svg or .png file.".to_owned())?;
        let rows =
            qr_rows(&link).ok_or_else(|| "The link does not fit in a QR code.".to_owned())?;
        let bytes = qr_image::render(&rows, format)
            .ok_or_else(|| "The QR code could not be drawn.".to_owned())?;
        // Written beside the target and renamed over it, so a write that fails
        // leaves an existing file as it was and no cut-off image behind.
        let temporary = format!("{path}.fetchpath-part");
        let saved = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .and_then(|mut file| std::io::Write::write_all(&mut file, &bytes))
            .and_then(|()| std::fs::rename(&temporary, &path));
        saved.map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            format!("Could not save the image: {error}")
        })
    })
    .await
}

fn new_share(result: CommandResult) -> Result<NewShare, String> {
    let CommandResult::ShareLink { link, share } = result else {
        return Err(unexpected(&result));
    };
    let link = link.expose().to_owned();
    let qr_rows = qr_rows(&link).unwrap_or_default();
    Ok(NewShare {
        link,
        share,
        qr_rows,
    })
}

/// Public share links and their setup (FP-117).
#[tauri::command]
pub(crate) async fn share_status(engine: Engine<'_>) -> Result<model::ShareView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || sharing_view(engine.send(Command::ShareStatus).map_err(text)?)).await
}

#[tauri::command]
pub(crate) async fn configure_sharing(
    base_url: String,
    port: Option<u16>,
    engine: Engine<'_>,
) -> Result<model::ShareView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        sharing_view(
            engine
                .send(Command::ConfigureSharing {
                    base_url: Some(base_url),
                    port,
                })
                .map_err(text)?,
        )
    })
    .await
}

/// The relay's enrollment key goes from the field to the engine and no
/// further: it is not kept here, not logged and not echoed in an error.
#[tauri::command]
pub(crate) async fn configure_relay(
    relay: bool,
    key: Option<String>,
    engine: Engine<'_>,
) -> Result<model::ShareView, String> {
    let key = key
        .filter(|key| !key.trim().is_empty())
        .map(fetchpath_protocol::RelayKey::try_from)
        .transpose()
        .map_err(|_| "That is not a relay key (64 hexadecimal characters).".to_owned())?;
    let engine = Arc::clone(&engine);
    off_thread(move || {
        sharing_view(
            engine
                .send(Command::ConfigureRelay { relay, key })
                .map_err(text)?,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn set_sharing_enabled(
    enabled: bool,
    engine: Engine<'_>,
) -> Result<model::ShareView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        sharing_view(
            engine
                .send(Command::SetSharingEnabled { enabled })
                .map_err(text)?,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn start_share_test(engine: Engine<'_>) -> Result<NewShare, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || new_share(engine.send(Command::StartShareTest).map_err(text)?)).await
}

#[tauri::command]
pub(crate) async fn create_share(
    path: String,
    expires_in_seconds: Option<u64>,
    engine: Engine<'_>,
) -> Result<NewShare, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        new_share(
            engine
                .send(Command::CreateShare {
                    source: OfferSource::Path { path },
                    expires_in_seconds,
                })
                .map_err(text)?,
        )
    })
    .await
}

/// One link by its handle, or every link when `handle` is empty.
#[tauri::command]
pub(crate) async fn revoke_share(
    handle: Option<String>,
    engine: Engine<'_>,
) -> Result<model::ShareView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let command = match handle {
            Some(handle) => Command::RevokeShare { handle },
            None => Command::RevokeAllShares,
        };
        sharing_view(engine.send(command).map_err(text)?)
    })
    .await
}
