use askama::Template;
use chrono::Local;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor, address::AddressError,
    message::Mailbox, transport::smtp::authentication::Credentials,
};

use crate::error::RbInternalError;

#[derive(Template)]
#[template(path = "verify_email.html")]
pub struct VerifyEmailTemplate<'a> {
    pub email: &'a str,
    pub verify_url: &'a str,
    pub time: &'a str,
}

pub struct EmailService {
    pub mailer: AsyncSmtpTransport<Tokio1Executor>,
    pub from_address: Mailbox,
}

impl EmailService {
    pub fn new(
        smtp_host: &str,
        smtp_user: &str,
        smtp_pass: &str,
        from_address: &str,
    ) -> Result<Self, AddressError> {
        let creds = Credentials::new(smtp_user.to_string(), smtp_pass.to_string());
        let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay(smtp_host)
            .unwrap()
            .credentials(creds)
            .build();

        Ok(Self {
            mailer,
            from_address: from_address.parse()?,
        })
    }

    pub async fn send_verify_email(
        &self,
        to: &str,
        verify_url: &str,
    ) -> Result<(), RbInternalError> {
        let template = VerifyEmailTemplate {
            email: to,
            verify_url,
            time: &Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        };

        let email_body = template.render()?;

        let email = Message::builder()
            .from(self.from_address.clone())
            .to(to.parse()?)
            .subject("验证你的邮箱")
            .header(lettre::message::header::ContentType::TEXT_HTML)
            .body(email_body)?;

        let to = to.to_string();
        let mailer = self.mailer.clone();
        tokio::spawn(async move {
            match mailer.send(email).await {
                Ok(_) => {
                    log::debug!("Sent verify email to {to}")
                }
                Err(e) => {
                    log::warn!("Failed to send verify email to {to} : {e}")
                }
            }
        });

        Ok(())
    }
}
