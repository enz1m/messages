//! Incremental SSE framing. Decode UTF-8 only after a complete line arrives;
//! network chunks are arbitrary byte boundaries, including within an emoji.

use crate::AdapterError;

const MAX_EVENT_BYTES: usize = 1024 * 1024;

pub(crate) struct Event {
    pub id: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub(crate) struct Decoder {
    line: Vec<u8>,
    data: String,
    id: Option<String>,
    after_cr: bool,
    started: bool,
    event_bytes: usize,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Event>, AdapterError> {
        let mut events = Vec::new();
        for byte in bytes {
            if self.after_cr && *byte == b'\n' {
                self.after_cr = false;
                continue;
            }
            self.after_cr = *byte == b'\r';
            if matches!(*byte, b'\r' | b'\n') {
                if let Some(event) = self.finish_line()? {
                    events.push(event);
                }
            } else {
                self.event_bytes += 1;
                if self.event_bytes > MAX_EVENT_BYTES {
                    return Err(AdapterError::InvalidData(
                        "Signal event exceeded the size limit".into(),
                    ));
                }
                self.line.push(*byte);
            }
        }
        Ok(events)
    }

    fn finish_line(&mut self) -> Result<Option<Event>, AdapterError> {
        let bytes = std::mem::take(&mut self.line);
        let mut line = std::str::from_utf8(&bytes)
            .map_err(|_| AdapterError::InvalidData("Signal event contains invalid UTF-8".into()))?;
        if !self.started {
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
            self.started = true;
        }
        if line.is_empty() {
            self.event_bytes = 0;
            return Ok(Some(Event {
                id: self.id.take(),
                data: std::mem::take(&mut self.data),
            }));
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "id" if !value.contains('\0') => self.id = Some(value.into()),
            "data" => {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value);
            }
            _ => {}
        }
        Ok(None)
    }
}

#[derive(Default)]
pub(crate) struct Connection {
    pub response: Option<reqwest::Response>,
    pub decoder: Decoder,
}
