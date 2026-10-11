/*
 * Copyright Stalwart Labs LLC See the COPYING
 * file at the top-level directory of this distribution.
 *
 * Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
 * https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
 * <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
 * option. This file may not be copied, modified, or distributed
 * except according to those terms.
 */

use super::Changes;
use crate::{
    event_source::{CalendarAlert, PushNotification},
    PushObject,
};

const MAX_EVENT_SIZE: usize = 1024 * 1024;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum EventType {
    Ping,
    #[default]
    State,
    CalendarAlert,
}

#[derive(Default, Debug)]
pub struct Event {
    pub event: EventType,
    pub id: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(Debug, Copy, Clone, Default)]
enum EventParserState {
    #[default]
    Init,
    Comment,
    Field,
    Value,
}

#[derive(Default, Debug)]
pub struct EventParser {
    state: EventParserState,
    field: Vec<u8>,
    value: Vec<u8>,
    bytes: Option<Vec<u8>>,
    pos: usize,
    result: Event,
}

impl EventParser {
    pub fn push_bytes(&mut self, bytes: Vec<u8>) {
        self.bytes = Some(bytes);
    }

    pub fn needs_bytes(&self) -> bool {
        self.bytes.is_none()
    }

    pub fn filter_notification(&mut self) -> Option<crate::Result<PushNotification>> {
        #[allow(clippy::never_loop)]
        #[allow(clippy::while_let_on_iterator)]
        while let Some(event) = self.next() {
            match event {
                Ok(Event {
                    event: EventType::State,
                    data,
                    id,
                    ..
                }) => {
                    return match serde_json::from_slice::<PushObject>(&data) {
                        Ok(PushObject::StateChange { changed }) => {
                            Some(Ok(PushNotification::StateChange(Changes {
                                id: if !id.is_empty() {
                                    Some(String::from_utf8(id).unwrap_or_default())
                                } else {
                                    None
                                },
                                changes: changed,
                            })))
                        }
                        Ok(unexpected) => Some(Err(crate::Error::Internal(format!(
                            "Unexpected PushObject variant: {:?}",
                            unexpected
                        )))),
                        Err(err) => Some(Err(err.into())),
                    };
                }
                Ok(Event {
                    event: EventType::CalendarAlert,
                    data,
                    ..
                }) => {
                    return match serde_json::from_slice::<CalendarAlert>(&data) {
                        Ok(calendar_alert) => {
                            Some(Ok(PushNotification::CalendarAlert(calendar_alert)))
                        }
                        Err(err) => Some(Err(err.into())),
                    };
                }
                Ok(Event {
                    event: EventType::Ping,
                    #[cfg(feature = "debug")]
                    id,
                    ..
                }) => {
                    #[cfg(feature = "debug")]
                    return Some(Ok(PushNotification::StateChange(Changes {
                        id: if !id.is_empty() {
                            Some(String::from_utf8(id).unwrap_or_default())
                        } else {
                            None
                        },
                        changes: ahash::AHashMap::from_iter([(
                            "ping".to_string(),
                            ahash::AHashMap::new(),
                        )]),
                    })));
                }
                Err(err) => return Some(Err(err)),
            }
        }
        None
    }
}

impl Iterator for EventParser {
    type Item = crate::Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.bytes.as_ref()?;

        for byte in bytes.get(self.pos..)? {
            self.pos += 1;

            match self.state {
                EventParserState::Init => match byte {
                    b':' => {
                        self.state = EventParserState::Comment;
                    }
                    b'\r' | b' ' => (),
                    b'\n' => {
                        return Some(Ok(std::mem::take(&mut self.result)));
                    }
                    _ => {
                        self.state = EventParserState::Field;
                        self.field.push(*byte);
                    }
                },
                EventParserState::Comment => {
                    if *byte == b'\n' {
                        self.state = EventParserState::Init;
                    }
                }
                EventParserState::Field => match byte {
                    b'\r' => (),
                    b'\n' => {
                        self.state = EventParserState::Init;
                        self.field.clear();
                    }
                    b':' => {
                        self.state = EventParserState::Value;
                    }
                    _ => {
                        if self.field.len() >= MAX_EVENT_SIZE {
                            return Some(Err(crate::Error::Internal(
                                "EventSource response is too long.".to_string(),
                            )));
                        }

                        self.field.push(*byte);
                    }
                },
                EventParserState::Value => match byte {
                    b'\r' => (),
                    b' ' if self.value.is_empty() => (),
                    b'\n' => {
                        self.state = EventParserState::Init;
                        match &self.field[..] {
                            b"id" => {
                                self.result.id.extend_from_slice(&self.value);
                            }
                            b"data" => {
                                self.result.data.extend_from_slice(&self.value);
                            }
                            b"event" => match &self.value[..] {
                                b"calendarAlert" => {
                                    self.result.event = EventType::CalendarAlert;
                                }
                                b"ping" => {
                                    self.result.event = EventType::Ping;
                                }
                                _ => {
                                    self.result.event = EventType::State;
                                }
                            },
                            _ => {
                                //ignore
                            }
                        }
                        self.field.clear();
                        self.value.clear();
                    }
                    _ => {
                        if (self.field.len() + self.value.len()) >= MAX_EVENT_SIZE {
                            return Some(Err(crate::Error::Internal(
                                "EventSource response is too long.".to_string(),
                            )));
                        }

                        self.value.push(*byte);
                    }
                },
            }
        }

        self.bytes = None;
        self.pos = 0;

        None
    }
}

#[cfg(test)]
mod tests {

    use super::{Event, EventType};

    #[derive(Debug, PartialEq, Eq)]
    struct EventString {
        event: EventType,
        id: String,
        data: String,
    }

    impl From<Event> for EventString {
        fn from(event: Event) -> Self {
            Self {
                event: event.event,
                id: String::from_utf8(event.id).unwrap(),
                data: String::from_utf8(event.data).unwrap(),
            }
        }
    }

    #[test]
    fn parse() {
        let mut parser = super::EventParser::default();
        let mut results = Vec::new();

        for frame in [
            Vec::from("event: state\nid:  0\ndata: test\n\n"),
            Vec::from("event: ping\nid:123\ndata: ping pa"),
            Vec::from("yload"),
            Vec::from("\n\n"),
            Vec::from(":comment\n\n"),
            Vec::from("data: YHOO\n"),
            Vec::from("data: +2\n"),
            Vec::from("data: 10\n\n"),
            Vec::from(": test stream\n"),
            Vec::from("data: first event\n"),
            Vec::from("id: 1\n\n"),
            Vec::from("data:second event\n"),
            Vec::from("id\n\n"),
            Vec::from("data:  third event\n\n"),
            Vec::from("data:hello\n\ndata: world\n\n"),
        ] {
            parser.push_bytes(frame);

            #[allow(clippy::while_let_on_iterator)]
            while let Some(event) = parser.next() {
                results.push(EventString::from(event.unwrap()));
            }
        }

        assert_eq!(
            results,
            vec![
                EventString {
                    event: EventType::State,
                    id: "0".to_string(),
                    data: "test".to_string()
                },
                EventString {
                    event: EventType::Ping,
                    id: "123".to_string(),
                    data: "ping payload".to_string()
                },
                EventString {
                    event: EventType::State,
                    id: "".to_string(),
                    data: "".to_string()
                },
                EventString {
                    event: EventType::State,
                    id: "".to_string(),
                    data: "YHOO+210".to_string()
                },
                EventString {
                    event: EventType::State,
                    id: "1".to_string(),
                    data: "first event".to_string()
                },
                EventString {
                    event: EventType::State,
                    id: "".to_string(),
                    data: "second event".to_string()
                },
                EventString {
                    event: EventType::State,
                    id: "".to_string(),
                    data: "third event".to_string()
                },
                EventString {
                    event: EventType::State,
                    id: "".to_string(),
                    data: "hello".to_string()
                },
                EventString {
                    event: EventType::State,
                    id: "".to_string(),
                    data: "world".to_string()
                }
            ]
        );
    }
}
