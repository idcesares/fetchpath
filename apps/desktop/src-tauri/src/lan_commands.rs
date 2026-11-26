//! Paired LAN sharing and device pairing Tauri commands (FP-033).

use crate::{Engine, off_thread, text, unexpected};
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model;
use std::sync::Arc;

fn lan_view(result: CommandResult) -> Result<model::LanView, String> {
    match result {
        CommandResult::Lan { lan } => Ok(lan),
        other => Err(unexpected(&other)),
    }
}

/// Paired computers and sharing, kept by the engine (FP-033).
#[tauri::command]
pub(crate) async fn lan_status(engine: Engine<'_>) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || lan_view(engine.send(Command::LanStatus).map_err(text)?)).await
}

#[tauri::command]
pub(crate) async fn set_lan_sharing(
    enabled: bool,
    engine: Engine<'_>,
) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        lan_view(
            engine
                .send(Command::SetLanSharing { enabled })
                .map_err(text)?,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn start_pairing(engine: Engine<'_>) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || lan_view(engine.send(Command::StartPairing).map_err(text)?)).await
}

#[tauri::command]
pub(crate) async fn cancel_pairing(engine: Engine<'_>) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || lan_view(engine.send(Command::CancelPairing).map_err(text)?)).await
}

#[tauri::command]
pub(crate) async fn join_pairing(
    address: String,
    code: String,
    label: Option<String>,
    engine: Engine<'_>,
) -> Result<model::PairedDevice, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let command = Command::JoinPairing {
            address,
            code,
            label: label.filter(|label| !label.trim().is_empty()),
        };
        match engine.send(command).map_err(text)? {
            CommandResult::Joined { device } => Ok(device),
            other => Err(unexpected(&other)),
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn unpair_device(
    key: String,
    engine: Engine<'_>,
) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || lan_view(engine.send(Command::Unpair { key }).map_err(text)?)).await
}
