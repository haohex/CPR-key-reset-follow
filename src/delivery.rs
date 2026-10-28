//! 自动通知与手动测试的邮件校验和有界 SMTP 传输
use crate::{
    host::fault,
    model::{SmtpSecurity, SmtpSettings},
};
use gateway_plugin_sdk::PluginFault;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    transport::smtp::authentication::Credentials,
};
use std::time::Duration;

pub fn validate(settings: &SmtpSettings) -> Result<(), PluginFault> {
    if settings.server.len() > 253
        || settings.username.len() > 320
        || settings.password.len() > 4096
        || settings.from.len() > 320
    {
        return Err(fault("SMTP 设置长度超出限制"));
    }
    if !settings.enabled {
        return Ok(());
    }
    validate_connection(settings)
}
pub fn validate_connection(settings: &SmtpSettings) -> Result<(), PluginFault> {
    if settings.server.trim().is_empty() || settings.port == 0 {
        return Err(fault("请填写 SMTP 服务器和有效端口"));
    }
    settings
        .from
        .parse::<lettre::message::Mailbox>()
        .map_err(|_| fault("发件地址格式不正确"))?;
    Ok(())
}
pub fn validate_recipient(recipient: &str) -> Result<(), PluginFault> {
    if recipient.len() > 320 {
        return Err(fault("收件地址过长"));
    }
    if !recipient.is_empty() {
        recipient
            .parse::<lettre::message::Mailbox>()
            .map_err(|_| fault("收件邮箱格式不正确"))?;
    }
    Ok(())
}
pub async fn send_email(
    settings: &SmtpSettings,
    recipient: &str,
    subject: &str,
    body: &str,
) -> Result<(), PluginFault> {
    validate_connection(settings)?;
    let message = Message::builder()
        .from(
            settings
                .from
                .parse()
                .map_err(|_| fault("发件地址格式不正确"))?,
        )
        .to(recipient.parse().map_err(|_| fault("收件地址格式不正确"))?)
        .subject(subject)
        .body(body.to_owned())
        .map_err(|_| fault("邮件内容构建失败"))?;
    let builder = match settings.security {
        SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.server),
        SmtpSecurity::Starttls => {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.server)
        }
    }
    .map_err(|_| fault("SMTP TLS 配置无效"))?;
    let mut builder = builder
        .port(settings.port)
        .timeout(Some(Duration::from_secs(6)));
    if !settings.username.is_empty() {
        builder = builder.credentials(Credentials::new(
            settings.username.clone(),
            settings.password.clone(),
        ));
    }
    builder
        .build()
        .send(message)
        .await
        .map_err(|_| fault("SMTP 发送失败，可能已投递，请核实后手动重试"))?;
    Ok(())
}
