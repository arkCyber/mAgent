//! SMTP client wrapper around `lettre`.
//!
//! We expose a single entry point, [`SmtpSession::send`], which
//! composes a minimal RFC 822 message and ships it via SMTP+STARTTLS
//! (or implicit TLS, depending on port). Reconnects happen lazily
//! — each call to `send` reuses the existing transport if it's
//! still alive.

use lettre::message::{header, Mailbox, Mailboxes, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::Config;

/// Cached SMTP transport. Holds a single async transport for the
/// lifetime of the MCP server process.
pub struct SmtpSession {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpSession {
    /// Connect to the SMTP server using STARTTLS (port 587) or
    /// implicit TLS (port 465). Authentication is plain / login
    /// depending on what the server advertises.
    pub async fn connect(config: &Config) -> Result<Self, anyhow::Error> {
        let creds = Credentials::new(config.user.clone(), config.password.clone());

        let transport = if config.smtp_port == 465 {
            // SMTPS — implicit TLS.
            AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host)?
                .port(config.smtp_port)
                .credentials(creds)
                .build()
        } else {
            // Submission — STARTTLS.
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)?
                .port(config.smtp_port)
                .credentials(creds)
                .build()
        };

        let from: Mailbox = config
            .user
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid from address `{}`: {e}", config.user))?;

        Ok(Self { transport, from })
    }

    /// Compose and send a plain-text email. Multiple recipients
    /// can be supplied as a comma-separated string.
    pub async fn send(&self, to: &str, subject: &str, body: &str) -> Result<String, anyhow::Error> {
        let addresses = parse_recipients(to)?;

        // multipart/alternative: text/plain + text/html version.
        let html_body = SinglePart::builder()
            .header(header::ContentType::parse("text/html; charset=utf-8")?)
            .body(html_escape(body));
        let multipart = MultiPart::alternative()
            .singlepart(SinglePart::plain(body.to_string()))
            .singlepart(html_body);

        // `MessageBuilder::to()` returns `Self`, but
        // `.multipart()` returns `Result<Message, _>` — once we
        // call `.multipart()` the value becomes a `Message`
        // (no more addresses can be added). So we collect all
        // recipients into a single `To(Mailboxes)` header up
        // front. `Mailboxes::with()` returns the updated
        // collection so we can chain.
        let builder = Message::builder()
            .from(self.from.clone())
            .mailbox(lettre::message::header::To::from(Mailboxes::from(
                addresses,
            )))
            .subject(subject.to_string())
            .multipart(multipart)?;

        let response = self.transport.send(builder).await?;
        if response.is_positive() {
            // Server returned a 2xx. We don't have `Display` on
            // the response struct, so we report a structured
            // summary and let the caller log the raw lines if
            // they want them.
            Ok(format!(
                "2xx ({} lines, code={})",
                response.message().count(),
                response.first_word().unwrap_or("ok")
            ))
        } else {
            Err(anyhow::anyhow!(
                "SMTP server rejected message (code={})",
                response.first_word().unwrap_or("unknown")
            ))
        }
    }
}

/// Parse a comma-separated recipient list using lettre's RFC-aware parser.
///
/// Hand-splitting on `,` broke legal display names that contain a comma
/// (`"Doe, John" <john@example.com>`), rejecting addresses the LLM can
/// legitimately produce.
fn parse_recipients(to: &str) -> Result<Vec<Mailbox>, anyhow::Error> {
    let addresses: Mailboxes = to
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid `to` address: {e}"))?;
    let addresses: Vec<Mailbox> = addresses.into();
    if addresses.is_empty() {
        anyhow::bail!("`to` must contain at least one address");
    }
    Ok(addresses)
}

/// Minimal HTML escaping for the auto-generated text/html part.
/// Covers &, <, > which are the only characters that can break
/// HTML structure.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipients_accept_a_comma_inside_a_display_name() {
        let to = r#""Doe, John" <john@example.com>, jane@example.com"#;
        let list = parse_recipients(to).expect("both addresses are valid");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn recipients_reject_an_empty_list() {
        assert!(parse_recipients("  ").is_err());
        assert!(parse_recipients("").is_err());
    }
}
