//! Push notifications to ntfy and a Discord webhook. The event, the rendering and the wire
//! shapes are `octo_core::notifications`.

pub mod discord_sink;
pub mod i_notification_sink;
pub mod notification_service;
pub mod ntfy_sink;

pub use discord_sink::DiscordSink;
pub use i_notification_sink::INotificationSink;
pub use notification_service::NotificationService;
pub use ntfy_sink::NtfySink;

/// Fails on a status that is not a success, in plain words: the admin test button shows the
/// message verbatim.
pub(crate) fn ensure_success_status_code(status: reqwest::StatusCode) -> anyhow::Result<()> {
    if status.is_success() {
        return Ok(());
    }
    anyhow::bail!(crate::services::http_client_factory::status_failure_message(
        status
    ))
}
