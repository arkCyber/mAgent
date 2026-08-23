//! SMTP client wrapper around `lettre`.
//!
//! We expose a single entry point, [`SmtpSession::send`], which
//! composes a minimal RFC 822 message and ships it via SMTP+STARTTLS
//! (or implicit TLS, depending on port). Reconnects happen lazily
//! — each call to `send` reuses the existing transport if it's
//! still alive.

use lettre::message::{header, Mailbox, MultiPart, SinglePart};
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
        let addresses: Vec<Mailbox> = to
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<Mailbox>())
            .collect::<Result<_, _>>()
            .map_err(|e| anyhow::anyhow!("invalid `to` address: {e}"))?;

        if addresses.is_empty() {
            anyhow::bail!("`to` must contain at least one address");
        }

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
        let mut mailboxes = lettre::message::Mailboxes::new();
        for mbox in addresses {
            mailboxes = mailboxes.with(mbox);
        }

        let builder = Message::builder()
            .from(self.from.clone())
            .mailbox(lettre::message::header::To::from(mailboxes))
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

/// Minimal HTML escaping for the auto-generated text/html part.
/// Covers &, <, > which are the only characters that can break
/// HTML structure.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
