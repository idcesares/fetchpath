//! Session records in protocol v1 terms.

use crate::{JobSnapshot as View, QueueRecord, Settings, retried_automatically, settings};
use fetchpath_protocol::error::{Action, ErrorCode, ErrorScope, ProtocolError};
use fetchpath_protocol::model::{
    self, EngineSettings, IntegrityOutcome, JobKind, JobState, Phase, Progress, SettingsView,
    Theme, WaitingReason,
};
use fetchpath_protocol::{JobId, Timestamp};

pub(crate) fn timestamp(ms: u64) -> Timestamp {
    Timestamp::from_unix_ms(i64::try_from(ms).unwrap_or(i64::MAX))
}

/// The contract's state for a session state. A scheduled job is queued with
/// a `not_before`; a job whose source must be refreshed is waiting for one.
pub(crate) fn job_state(state: &str) -> JobState {
    match state {
        "queued" | "scheduled" => JobState::Queued,
        "running" => JobState::Running,
        "paused" => JobState::Paused,
        "cancelling" => JobState::Cancelling,
        "cancelled" => JobState::Cancelled,
        "completed" => JobState::Completed,
        "failed" => JobState::Failed,
        "needs_source" => JobState::WaitingForSource,
        _ => JobState::Unknown,
    }
}

pub(crate) fn waiting_reason(view: &View) -> Option<WaitingReason> {
    (view.state == "needs_source").then_some(match view.action.as_deref() {
        Some("recapture") => WaitingReason::BrowserContextLost,
        _ => WaitingReason::SourceExpired,
    })
}

pub(crate) fn action(action: &str) -> Action {
    match action {
        "retry" => Action::Retry,
        "edit_link" => Action::EditLink,
        "recapture" => Action::Recapture,
        "choose_new_path" => Action::ChooseNewPath,
        "check_checksum" => Action::CheckChecksum,
        "configure_media_tools" => Action::ConfigureMediaTools,
        "refresh_source" => Action::RefreshSource,
        _ => Action::Unknown,
    }
}

fn code(text: &str) -> ErrorCode {
    ErrorCode::try_from(text.to_owned()).unwrap_or(ErrorCode::INTERNAL_UNKNOWN)
}

/// The error a client acts on: its code and action, with the message as
/// fallback text only (finding F3). A failure is `retryable` only when the
/// queue itself would retry it (finding F2).
pub(crate) fn protocol_error(view: &View) -> Option<ProtocolError> {
    let (code_text, retryable) = match view.state.as_str() {
        "failed" => {
            let code_text = view
                .error_code
                .clone()
                .or_else(|| crate::failure_code(view))
                .unwrap_or_else(|| "internal.unknown".into());
            let retryable =
                view.action.as_deref() == Some("retry") && retried_automatically(&code_text);
            (code_text, retryable)
        }
        "needs_source" => (
            match view.action.as_deref() {
                Some("recapture") => "auth.browser_context_lost".to_owned(),
                _ => "source.link_expired".to_owned(),
            },
            false,
        ),
        _ => return None,
    };
    let mut error = ProtocolError::new(
        code(&code_text),
        ErrorScope::Job,
        view.error.clone().unwrap_or_default(),
    );
    error.retryable = retryable;
    error.action = view.action.as_deref().map(action);
    Some(error)
}

fn integrity(view: &View) -> Option<IntegrityOutcome> {
    match view.state.as_str() {
        "completed" if view.expected_sha256.is_some() => Some(IntegrityOutcome::VerifiedExpected),
        "completed" if view.observed_sha256.is_some() => Some(IntegrityOutcome::DownloadedObserved),
        "failed" if view.error_code.as_deref() == Some("integrity.checksum_mismatch") => {
            Some(IntegrityOutcome::VerificationFailed)
        }
        _ => None,
    }
}

/// One job as a protocol snapshot, reflecting its last durable event.
pub(crate) fn snapshot(record: &QueueRecord) -> model::JobSnapshot {
    let view = &record.view;
    model::JobSnapshot {
        job_id: JobId::try_from(record.id.as_str()).unwrap_or_else(|_| JobId::random()),
        kind: match view.kind.as_str() {
            "file" => JobKind::File,
            "media" => JobKind::Media,
            _ => JobKind::Unknown,
        },
        state: job_state(&view.state),
        waiting_reason: waiting_reason(view),
        job_revision: record.durable.job_revision,
        last_seq: record.durable.last_seq,
        source_display: view.source.clone(),
        destination: view.destination.clone(),
        progress: progress(view),
        integrity: integrity(view),
        expected_sha256: view.expected_sha256.clone(),
        observed_sha256: view.observed_sha256.clone(),
        cleanup_pending: view.cleanup_pending,
        error: protocol_error(view),
        attempt: view.attempt,
        retry_at: record.retry_at_ms.map(timestamp),
        created_at: timestamp(view.created_at_ms),
        not_before: view.not_before_ms.map(timestamp),
        finished_at: view.finished_at_ms.map(timestamp),
        quality_label: view.quality_label.clone(),
    }
}

pub(crate) fn progress(view: &View) -> Progress {
    Progress {
        phase: (view.state == "running").then_some(Phase::Receive),
        bytes_received: view.bytes_received,
        bytes_checkpointed: None,
        bytes_verified: None,
        bytes_total: view.total_bytes,
        rate_bytes_per_second: view.bytes_per_second,
        eta_seconds: view.eta_seconds,
        confidence: None,
    }
}

pub(crate) fn engine_settings(settings: &Settings) -> EngineSettings {
    EngineSettings {
        max_active_downloads: settings.max_active_downloads as u64,
        default_destination_dir: settings.default_destination_dir.clone(),
        auto_retry: settings.auto_retry,
        auto_retry_max_attempts: settings.auto_retry_max_attempts,
        auto_retry_base_delay_seconds: settings.auto_retry_base_delay_seconds,
        close_to_tray: settings.close_to_tray,
        power_mode: settings.power_mode,
        media_tools_dir: settings.media_tools_dir.clone(),
        confirm_remove_completed: settings.confirm_remove_completed,
        theme: match settings.theme {
            settings::Theme::System => Theme::System,
            settings::Theme::Light => Theme::Light,
            settings::Theme::Dark => Theme::Dark,
        },
        onboarding_completed: settings.onboarding_completed,
        start_engine_at_sign_in: settings.start_engine_at_sign_in,
    }
}

/// Settings from the wire. A theme this build does not know keeps the
/// current one; every value is clamped by the session.
pub(crate) fn session_settings(wire: &EngineSettings, current: &Settings) -> Settings {
    Settings {
        max_active_downloads: usize::try_from(wire.max_active_downloads).unwrap_or(usize::MAX),
        default_destination_dir: wire.default_destination_dir.clone(),
        auto_retry: wire.auto_retry,
        auto_retry_max_attempts: wire.auto_retry_max_attempts,
        auto_retry_base_delay_seconds: wire.auto_retry_base_delay_seconds,
        close_to_tray: wire.close_to_tray,
        power_mode: wire.power_mode,
        media_tools_dir: wire.media_tools_dir.clone(),
        confirm_remove_completed: wire.confirm_remove_completed,
        theme: match wire.theme {
            Theme::System => settings::Theme::System,
            Theme::Light => settings::Theme::Light,
            Theme::Dark => settings::Theme::Dark,
            Theme::Unknown => current.theme,
        },
        onboarding_completed: wire.onboarding_completed,
        start_engine_at_sign_in: wire.start_engine_at_sign_in,
    }
}

pub(crate) fn settings_view(settings: &Settings, repaired: bool) -> SettingsView {
    SettingsView {
        settings: engine_settings(settings),
        repaired,
        max_active_limit: settings::MAX_ACTIVE_DOWNLOADS as u64,
        max_retry_attempts: settings::MAX_RETRY_ATTEMPTS,
    }
}
