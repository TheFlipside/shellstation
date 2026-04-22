use std::time::Duration;

use regex::Regex;

use crate::db::models::{LoginSequence, LoginSequenceStep};

const STEP_TIMEOUT: Duration = Duration::from_secs(10);
const MATCH_BUFFER_MAX: usize = 65536;

pub enum ResponseSegment {
    Data(Vec<u8>),
    Pause(Duration),
}

pub struct ResolvedStep {
    pub pattern: Regex,
    pub response_segments: Vec<ResponseSegment>,
    pub append_cr: bool,
}

pub struct LoginSequenceConfig {
    pub send_initial_cr: bool,
    pub steps: Vec<ResolvedStep>,
}

fn expand_response(raw: &str, username: &str, password: &str) -> Vec<ResponseSegment> {
    let mut segments: Vec<ResponseSegment> = Vec::new();
    let mut current_data: Vec<u8> = Vec::new();

    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'r' => current_data.push(0x0D),
                b'n' => current_data.push(0x0A),
                b'e' => current_data.push(0x1B),
                b't' => current_data.push(0x09),
                b'b' => current_data.push(0x08),
                b'\\' => current_data.push(0x5C),
                b'p' => {
                    if !current_data.is_empty() {
                        segments.push(ResponseSegment::Data(std::mem::take(&mut current_data)));
                    }
                    segments.push(ResponseSegment::Pause(Duration::from_secs(1)));
                }
                b's' => current_data.extend_from_slice(username.as_bytes()),
                b'w' => current_data.extend_from_slice(password.as_bytes()),
                other => {
                    current_data.push(b'\\');
                    current_data.push(other);
                }
            }
            i += 2;
        } else {
            current_data.push(bytes[i]);
            i += 1;
        }
    }

    if !current_data.is_empty() {
        segments.push(ResponseSegment::Data(current_data));
    }

    segments
}

fn validate_step(step: &LoginSequenceStep) -> Result<Regex, String> {
    // Safe from ReDoS: the `regex` crate uses Thompson's NFA, guaranteeing linear-time matching.
    Regex::new(&step.pattern).map_err(|e| format!("Invalid regex pattern '{}': {e}", step.pattern))
}

pub fn resolve_login_sequence(
    sequence: &LoginSequence,
    username: &str,
    password: &str,
) -> Result<LoginSequenceConfig, String> {
    let mut steps = Vec::with_capacity(sequence.steps.len());
    for step in &sequence.steps {
        let pattern = validate_step(step)?;
        let response_segments = expand_response(&step.response, username, password);
        steps.push(ResolvedStep {
            pattern,
            response_segments,
            append_cr: step.append_cr,
        });
    }
    Ok(LoginSequenceConfig {
        send_initial_cr: sequence.send_initial_cr,
        steps,
    })
}

pub use run::*;

mod run {
    use super::*;
    use base64::Engine;
    use tauri::{AppHandle, Emitter};
    use tracing::debug;

    pub struct LoginSequenceIO<'a> {
        pub config: LoginSequenceConfig,
        pub event_name: &'a str,
        pub app: &'a AppHandle,
        pub session_id: &'a str,
        pub logger:
            Option<&'a std::sync::Arc<std::sync::Mutex<crate::session_logger::SessionLogManager>>>,
    }

    // Emits data received FROM the server (prompts, banners) — never the credentials we send.
    fn emit_output(io: &LoginSequenceIO<'_>, data: &[u8]) {
        if let Some(lg) = io.logger {
            if let Ok(mut mgr) = lg.lock() {
                mgr.write_log(io.session_id, data);
            }
        }
        let payload = base64::prelude::BASE64_STANDARD.encode(data);
        if let Err(e) = io.app.emit(io.event_name, &payload) {
            debug!(session_id = %io.session_id, error = %e, "Login sequence: failed to emit output");
        }
    }

    #[async_trait::async_trait]
    pub trait LoginSequenceTransport: Send {
        async fn read(&mut self) -> Option<Vec<u8>>;
        async fn write(&mut self, data: &[u8]) -> Result<(), String>;
    }

    async fn send_response_segments(
        segments: &[ResponseSegment],
        append_cr: bool,
        transport: &mut dyn LoginSequenceTransport,
        session_id: &str,
    ) -> bool {
        for seg in segments {
            match seg {
                ResponseSegment::Data(data) => {
                    if let Err(e) = transport.write(data).await {
                        debug!(session_id = %session_id, error = %e, "Login sequence: write failed");
                        return false;
                    }
                }
                ResponseSegment::Pause(dur) => {
                    tokio::time::sleep(*dur).await;
                }
            }
        }
        if append_cr {
            if let Err(e) = transport.write(&[0x0D]).await {
                debug!(session_id = %session_id, error = %e, "Login sequence: write failed");
                return false;
            }
        }
        true
    }

    pub async fn run_login_sequence(
        io: LoginSequenceIO<'_>,
        transport: &mut dyn LoginSequenceTransport,
    ) {
        if io.config.send_initial_cr {
            debug!(session_id = %io.session_id, "Login sequence: sending initial CR");
            if let Err(e) = transport.write(&[0x0D]).await {
                debug!(session_id = %io.session_id, error = %e, "Login sequence: write failed");
                return;
            }
        }

        let mut match_buffer = Vec::new();

        for (idx, step) in io.config.steps.iter().enumerate() {
            debug!(
                session_id = %io.session_id,
                step = idx,
                pattern = %step.pattern,
                "Login sequence: waiting for match"
            );

            let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
            let mut matched = false;

            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    debug!(
                        session_id = %io.session_id,
                        step = idx,
                        "Login sequence: step timed out, skipping"
                    );
                    break;
                }

                match tokio::time::timeout(remaining, transport.read()).await {
                    Ok(Some(data)) => {
                        emit_output(&io, &data);

                        let space = MATCH_BUFFER_MAX.saturating_sub(match_buffer.len());
                        if data.len() <= space {
                            match_buffer.extend_from_slice(&data);
                        } else if space > 0 {
                            match_buffer.extend_from_slice(&data[..space]);
                        }

                        let text = String::from_utf8_lossy(&match_buffer);
                        if step.pattern.is_match(&text) {
                            debug!(
                                session_id = %io.session_id,
                                step = idx,
                                "Login sequence: pattern matched, sending response"
                            );
                            let ok = send_response_segments(
                                &step.response_segments,
                                step.append_cr,
                                transport,
                                io.session_id,
                            )
                            .await;
                            if !ok {
                                return;
                            }
                            matched = true;
                            break;
                        }
                    }
                    Ok(None) => {
                        debug!(
                            session_id = %io.session_id,
                            "Login sequence: channel closed during automation"
                        );
                        return;
                    }
                    Err(_) => {
                        debug!(
                            session_id = %io.session_id,
                            step = idx,
                            "Login sequence: step timed out, skipping"
                        );
                        break;
                    }
                }
            }

            if matched {
                match_buffer.clear();
            }
        }

        debug!(session_id = %io.session_id, "Login sequence: completed");
    }

    pub struct SshChannelTransport<'a> {
        pub channel: &'a mut russh::Channel<russh::client::Msg>,
    }

    #[async_trait::async_trait]
    impl LoginSequenceTransport for SshChannelTransport<'_> {
        async fn read(&mut self) -> Option<Vec<u8>> {
            use russh::ChannelMsg;
            loop {
                match self.channel.wait().await {
                    Some(ChannelMsg::Data { ref data })
                    | Some(ChannelMsg::ExtendedData { ref data, ext: 1 }) => {
                        return Some(data.to_vec());
                    }
                    Some(ChannelMsg::Eof | ChannelMsg::Close) => return None,
                    Some(_) => continue,
                    None => return None,
                }
            }
        }

        async fn write(&mut self, data: &[u8]) -> Result<(), String> {
            self.channel
                .data(data)
                .await
                .map_err(|e| format!("SSH write error: {e}"))
        }
    }
}
