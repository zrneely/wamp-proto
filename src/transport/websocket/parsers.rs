use std::collections::HashMap;

use failure::Fail;
use serde_json::Value;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::{
    proto::rx::{self, ReceivedMessage as _, RxMessage},
    transport::TransportableValue as TV,
    uri::Uri,
    Id, MAX_ID_VAL,
};

mod helpers {
    use super::*;

    fn json_to_tv(value: &Value) -> Option<TV> {
        Some(match value {
            Value::Null => return None,
            Value::Bool(val) => TV::Bool(*val),
            Value::Number(num) => {
                if let Some(val) = num.as_u64() {
                    TV::Integer(val)
                } else {
                    warn!("Skipping negative or floating point number {:?}", num);
                    return None;
                }
            }
            Value::String(val) => TV::String(val.clone()),
            Value::Array(vals) => TV::List(vals.iter().filter_map(json_to_tv).collect()),
            Value::Object(vals) => {
                let mut result = HashMap::<String, TV>::new();
                for (k, v) in vals {
                    if let Some(val) = json_to_tv(v) {
                        result.insert(k.clone(), val);
                    }
                }
                TV::Dict(result)
            }
        })
    }

    pub fn parse_details(value: &Value) -> Result<HashMap<String, TV>, MessageParseError> {
        if let Some(TV::Dict(dict)) = json_to_tv(value) {
            Ok(dict)
        } else {
            warn!("details not dictionary: {:?}", value);
            Err(MessageParseError::DetailsNotDictionary)
        }
    }

    pub fn parse_id<S>(value: &Value) -> Result<Id<S>, MessageParseError> {
        if let Some(id) = value.as_u64() {
            if id <= MAX_ID_VAL {
                Ok(Id::<S>::from_raw_value(id))
            } else {
                warn!("ID out of range: {}", id);
                Err(MessageParseError::IdOutOfRange)
            }
        } else {
            warn!("ID is not integer: {}", value);
            Err(MessageParseError::IdNotInteger)
        }
    }

    pub fn parse_uri(value: &Value) -> Result<Uri, MessageParseError> {
        if let Some(uri_str) = value.as_str() {
            if let Some(reason) = Uri::relaxed(uri_str) {
                Ok(reason)
            } else {
                warn!("Bad URI: {:?}", uri_str);
                Err(MessageParseError::BadUri)
            }
        } else {
            warn!("reason not string: {:?}", value);
            Err(MessageParseError::UriNotString)
        }
    }

    pub fn parse_arguments(value: &Value) -> Result<Vec<TV>, MessageParseError> {
        if let Some(TV::List(arguments)) = json_to_tv(value) {
            Ok(arguments)
        } else {
            warn!("arguments not list: {:?}", value);
            Err(MessageParseError::ArgumentsNotList)
        }
    }

    pub fn parse_arguments_kw(value: &Value) -> Result<HashMap<String, TV>, MessageParseError> {
        if let Some(TV::Dict(arguments_kw)) = json_to_tv(value) {
            Ok(arguments_kw)
        } else {
            warn!("arguments_kw not dictionary: {:?}", value);
            Err(MessageParseError::ArguementsKwNotDictionary)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::GlobalScope;

        #[test]
        fn json_to_tv_test() {
            assert_eq!(Some(TV::Integer(4)), json_to_tv(&json!(4)));
            assert_eq!(Some(TV::Integer(0)), json_to_tv(&json!(0)));
            assert_eq!(
                Some(TV::Integer(0x7FFF_FFFF_FFFF_FFFF)),
                json_to_tv(&json!(0x7FFF_FFFF_FFFF_FFFFu64))
            );
            assert_eq!(None, json_to_tv(&json!(-1)));
            assert_eq!(None, json_to_tv(&json!(-3)));

            assert_eq!(
                Some(TV::List(vec![
                    TV::Integer(1),
                    TV::Integer(2),
                    TV::String("asdf".into())
                ])),
                json_to_tv(&json!([1, 2, "asdf"]))
            );
        }

        #[test]
        fn parse_details_test() {
            assert!(parse_details(&json!(5)).is_err());
            assert!(parse_details(&json!("asdf")).is_err());
            assert!(parse_details(&json!([1, 2, "asdf"])).is_err());

            let result = parse_details(&json!({
                "a": 1234,
                "b": [2, 4],
            }));

            let mut expected_result = HashMap::new();
            expected_result.insert("a".into(), TV::Integer(1234));
            expected_result.insert("b".into(), TV::List(vec![TV::Integer(2), TV::Integer(4)]));

            assert_eq!(expected_result, result.unwrap());
        }

        #[test]
        fn parse_id_test() {
            assert!(parse_id::<GlobalScope>(&json!("asdf")).is_err());
            assert!(parse_id::<GlobalScope>(&json!([1, 2])).is_err());
            assert_eq!(
                Id::<GlobalScope>::from_raw_value(1234),
                parse_id::<GlobalScope>(&json!(1234)).unwrap()
            );
            assert_eq!(
                Id::<GlobalScope>::from_raw_value(MAX_ID_VAL),
                parse_id::<GlobalScope>(&json!(MAX_ID_VAL)).unwrap()
            );
            assert!(parse_id::<GlobalScope>(&json!(MAX_ID_VAL + 1)).is_err());
        }

        #[test]
        fn parse_uri_test() {
            assert!(parse_uri(&json!(1)).is_err());
            assert!(parse_uri(&json!([1, 2, 3])).is_err());
            assert_eq!(
                Uri::raw("asdf.ghjk"),
                parse_uri(&json!("asdf.ghjk")).unwrap()
            );
            assert!(parse_uri(&json!("a.#.b")).is_err());
        }

        #[test]
        fn parse_arguments_test() {
            assert!(parse_arguments(&json!("asdf")).is_err());
            assert!(parse_arguments(&json!({
                "a": 1, "b": [1, "asdf"]
            }))
            .is_err());

            let result = parse_arguments(&json!(["asdf", 1, 4])).unwrap();
            let expected_result = vec![TV::String("asdf".into()), TV::Integer(1), TV::Integer(4)];
            assert_eq!(expected_result, result);
        }

        #[test]
        fn parse_arguments_kw_test() {
            assert!(parse_arguments_kw(&json!(5)).is_err());
            assert!(parse_arguments_kw(&json!("asdf")).is_err());
            assert!(parse_arguments_kw(&json!([1, 2, "asdf"])).is_err());

            let result = parse_arguments_kw(&json!({
                "a": 1234,
                "b": [2, 4],
            }));

            let mut expected_result = HashMap::new();
            expected_result.insert("a".into(), TV::Integer(1234));
            expected_result.insert("b".into(), TV::List(vec![TV::Integer(2), TV::Integer(4)]));

            assert_eq!(expected_result, result.unwrap());
        }
    }
}

#[derive(Debug, Fail)]
pub enum MessageParseError {
    #[fail(display = "received bad JSON: {}", 0)]
    BadJson(serde_json::Error),

    #[fail(display = "received non-array JSON")]
    NonArrayJson,

    #[fail(display = "received empty array")]
    EmptyArray,

    #[fail(display = "received non-integer message code")]
    NonIntegerMessageCode,

    #[fail(display = "received message with bad length")]
    BadMessageLength,

    #[fail(display = "received non-integer ID")]
    IdNotInteger,

    #[fail(display = "recieved out-of-range ID")]
    IdOutOfRange,

    #[fail(display = "received non-dictionary details")]
    DetailsNotDictionary,

    #[fail(display = "received invalid URI")]
    BadUri,

    #[fail(display = "received non-string URI")]
    UriNotString,

    #[fail(display = "received non-integer request type in ERROR message")]
    ErrorRequestTypeNotInt,

    #[fail(display = "received non-list arguments")]
    ArgumentsNotList,

    #[fail(display = "received non-dictionary keyword arguments")]
    ArguementsKwNotDictionary,
}

// Outputs Ok(None) if the message should be skipped (ie, non-text message or unknown message code)
pub fn parse_message(message: Message) -> Result<Option<RxMessage>, MessageParseError> {
    if let Message::Text(message_text) = message {
        match serde_json::from_str(&message_text) {
            Ok(Value::Array(values)) => match values.get(0) {
                Some(msg_code_value) => match msg_code_value.as_u64() {
                    Some(msg_code) => {
                        trace!("Received WAMP message: {:?}", values);
                        match msg_code {
                            rx::Welcome::MSG_CODE => {
                                Ok(Some(RxMessage::Welcome(parse_welcome(&values[1..])?)))
                            }
                            rx::Abort::MSG_CODE => {
                                Ok(Some(RxMessage::Abort(parse_abort(&values[1..])?)))
                            }
                            rx::Error::MSG_CODE => {
                                Ok(Some(RxMessage::Error(parse_error(&values[1..])?)))
                            }
                            rx::Goodbye::MSG_CODE => {
                                Ok(Some(RxMessage::Goodbye(parse_goodbye(&values[1..])?)))
                            }
                            rx::Subscribed::MSG_CODE => {
                                Ok(Some(RxMessage::Subscribed(parse_subscribed(&values[1..])?)))
                            }
                            rx::Unsubscribed::MSG_CODE => Ok(Some(RxMessage::Unsubscribed(
                                parse_unsubscribed(&values[1..])?,
                            ))),
                            rx::Event::MSG_CODE => {
                                Ok(Some(RxMessage::Event(parse_event(&values[1..])?)))
                            }

                            msg_code => {
                                warn!("Received unknown message code: {}", msg_code);
                                Ok(None)
                            }
                        }
                    }
                    None => {
                        warn!("Received non-integer message code: {}", msg_code_value);
                        Err(MessageParseError::NonIntegerMessageCode)
                    }
                },
                None => {
                    warn!("Received empty array");
                    Err(MessageParseError::EmptyArray)
                }
            },
            Ok(bad_value) => {
                warn!("Received non-array JSON: {:?}", bad_value);
                Err(MessageParseError::NonArrayJson)
            }
            Err(json_error) => {
                warn!(
                    "Received bad JSON: {:?} (inner error: {})",
                    message_text, json_error
                );
                Err(MessageParseError::BadJson(json_error))
            }
        }
    } else {
        trace!("Received non-text message: {:?}", message);
        Ok(None)
    }
}

fn parse_welcome(msg: &[Value]) -> Result<rx::Welcome, MessageParseError> {
    if msg.len() != 2 {
        warn!("Bad WELCOME message length: {}", msg.len());
        return Err(MessageParseError::BadMessageLength);
    }

    let session = helpers::parse_id(&msg[0])?;
    let details = helpers::parse_details(&msg[1])?;

    let welcome = rx::Welcome { session, details };
    trace!("Received WELCOME message: {:?}", welcome);
    Ok(welcome)
}

fn parse_abort(msg: &[Value]) -> Result<rx::Abort, MessageParseError> {
    if msg.len() != 2 {
        warn!("Bad ABORT message length: {}", msg.len());
        return Err(MessageParseError::BadMessageLength);
    }

    let details = helpers::parse_details(&msg[0])?;
    let reason = helpers::parse_uri(&msg[1])?;

    let abort = rx::Abort { details, reason };
    trace!("Received ABORT message: {:?}", abort);
    Ok(abort)
}

fn parse_error(msg: &[Value]) -> Result<rx::Error, MessageParseError> {
    if msg.len() < 4 || msg.len() > 6 {
        warn!("Bad ERROR message length: {}", msg.len());
        return Err(MessageParseError::BadMessageLength);
    }

    let request_type = msg[0]
        .as_u64()
        .ok_or(MessageParseError::ErrorRequestTypeNotInt)?;

    let request = helpers::parse_id(&msg[1])?;
    let details = helpers::parse_details(&msg[2])?;
    let error = helpers::parse_uri(&msg[3])?;

    let arguments = msg
        .get(4)
        .map(|list| helpers::parse_arguments(list))
        .transpose()?;

    let arguments_kw = msg
        .get(5)
        .map(|dict| helpers::parse_arguments_kw(dict))
        .transpose()?;

    let error = rx::Error {
        request_type,
        request,
        details,
        error,
        arguments,
        arguments_kw,
    };
    trace!("Received ERROR message: {:?}", error);
    Ok(error)
}

fn parse_goodbye(msg: &[Value]) -> Result<rx::Goodbye, MessageParseError> {
    if msg.len() != 2 {
        warn!("Bad GOODBYE message length: {}", msg.len());
        return Err(MessageParseError::BadMessageLength);
    }

    let details = helpers::parse_details(&msg[0])?;
    let reason = helpers::parse_uri(&msg[1])?;

    let goodbye = rx::Goodbye { details, reason };
    trace!("Received GOODBYE message: {:?}", goodbye);
    Ok(goodbye)
}

fn parse_subscribed(msg: &[Value]) -> Result<rx::Subscribed, MessageParseError> {
    if msg.len() != 2 {
        warn!("Bad SUBSCRIBED message length: {}", msg.len());
        return Err(MessageParseError::BadMessageLength);
    }

    let request = helpers::parse_id(&msg[0])?;
    let subscription = helpers::parse_id(&msg[1])?;

    let subscribed = rx::Subscribed {
        request,
        subscription,
    };
    trace!("Received SUBSCRIBED message: {:?}", subscribed);
    Ok(subscribed)
}

fn parse_unsubscribed(msg: &[Value]) -> Result<rx::Unsubscribed, MessageParseError> {
    if msg.len() != 1 {
        warn!("Bad UNSUBSCRIBED message length: {}", msg.len());
        return Err(MessageParseError::BadMessageLength);
    }

    let request = helpers::parse_id(&msg[0])?;

    let unsubscribed = rx::Unsubscribed { request };
    trace!("Received UNSUBSCRIBED message: {:?}", unsubscribed);
    Ok(unsubscribed)
}

fn parse_event(msg: &[Value]) -> Result<rx::Event, MessageParseError> {
    if msg.len() < 3 || msg.len() > 5 {
        warn!("Bad EVENT message length: {}", msg.len());
        return Err(MessageParseError::BadMessageLength);
    }

    let subscription = helpers::parse_id(&msg[0])?;
    let publication = helpers::parse_id(&msg[1])?;
    let details = helpers::parse_details(&msg[2])?;

    let arguments = msg
        .get(3)
        .map(|list| helpers::parse_arguments(list))
        .transpose()?;

    let arguments_kw = msg
        .get(4)
        .map(|dict| helpers::parse_arguments_kw(dict))
        .transpose()?;

    let event = rx::Event {
        subscription,
        publication,
        details,
        arguments,
        arguments_kw,
    };
    trace!("Received EVENT message: {:?}", event);
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GlobalScope, RouterScope, SessionScope};

    #[test]
    fn parse_mssage_test() {
        println!("Scenario 0: received non-Text messages");
        assert!(parse_message(Message::Binary(vec![0; 10]))
            .unwrap()
            .is_none());
        assert!(parse_message(Message::Close(None)).unwrap().is_none());
        assert!(parse_message(Message::Ping(vec![0; 10])).unwrap().is_none());
        assert!(parse_message(Message::Pong(vec![0; 10])).unwrap().is_none());

        println!("Scenario 1: happy path received WELCOME");
        assert_eq!(
            RxMessage::Welcome(rx::Welcome {
                session: Id::<GlobalScope>::from_raw_value(12345),
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::Integer(1));
                    details.insert("y".into(), TV::Bool(true));
                    details
                }
            }),
            parse_message(r#"[2,12345,{"x":1,"y":true}]"#.into())
                .unwrap()
                .unwrap()
        );

        println!("Scenario 2: received non-JSON text");
        assert!(parse_message(r#"~!@#$%^&*("#.into()).is_err());

        println!("Scenario 3: received non-array JSON");
        assert!(parse_message(r#"{"x":1}"#.into()).is_err());

        println!("Scenario 4: received empty array");
        assert!(parse_message(r#"[]"#.into()).is_err());

        println!("Scenario 5: received non-numeric message code");
        assert!(parse_message(r#"["foobar", 1234, 2345]"#.into()).is_err());

        println!("Scenario 6: received unknown message code");
        assert!(parse_message(r#"[123456, 1234]"#.into()).unwrap().is_none());
    }

    #[test]
    fn parse_welcome_test() {
        println!("Scenario 0: happy path");
        assert_eq!(
            rx::Welcome {
                session: Id::<GlobalScope>::from_raw_value(1234),
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::Integer(1));
                    details.insert("y".into(), TV::Bool(true));
                    details
                }
            },
            parse_welcome(&[json!(1234), json!({"x": 1, "y": true})]).unwrap()
        );

        println!("Scenario 1: fail to parse session ID");
        assert!(parse_welcome(&[json!("asdf"), json!({"x": 1, "y": true})]).is_err());

        println!("Scenario 2: fail to parse details");
        assert!(parse_welcome(&[json!(12345), json!("foobar")]).is_err());

        println!("Scenario 3: not enough arguments");
        assert!(parse_welcome(&[json!(12345)]).is_err());

        println!("Scenario 4: too many arguments");
        assert!(
            parse_welcome(&[json!(12345), json!({"x": 1, "y": true}), json!("foobar")]).is_err()
        );
    }

    #[test]
    fn parse_abort_test() {
        println!("Scenario 0: happy path");
        assert_eq!(
            rx::Abort {
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::Integer(1));
                    details.insert("y".into(), TV::Bool(true));
                    details
                },
                reason: Uri::raw("org.foo.bar.error"),
            },
            parse_abort(&[json!({"x": 1, "y": true}), json!("org.foo.bar.error")]).unwrap()
        );

        println!("Scenario 1: fail to parse details");
        assert!(parse_abort(&[json!("foobar"), json!("org.foo.bar.error")]).is_err());

        println!("Scenario 2: fail to parse reason (string)");
        assert!(parse_abort(&[json!({"x": 1, "y": true}), json!(12345)]).is_err());

        println!("Scenario 3: fail to parse reason (uri)");
        assert!(parse_abort(&[json!({"x": 1, "y": true}), json!("..")]).is_err());

        println!("Scenario 4: not enough arguments");
        assert!(parse_abort(&[json!({"x": 1, "y": true})]).is_err());

        println!("Scenario 5: too many arguments");
        assert!(parse_abort(&[
            json!({"x": 1, "y": true}),
            json!("org.foo.bar.error"),
            json!(12345),
        ])
        .is_err());
    }

    #[test]
    fn parse_error_test() {
        println!("Scenario 0: happy path (no args)");
        assert_eq!(
            rx::Error {
                request_type: 1234,
                request: Id::<SessionScope>::from_raw_value(2345),
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::Integer(1));
                    details
                },
                error: Uri::raw("org.foo.bar.error"),
                arguments: None,
                arguments_kw: None,
            },
            parse_error(&[
                json!(1234),
                json!(2345),
                json!({"x": 1}),
                json!("org.foo.bar.error")
            ])
            .unwrap()
        );

        println!("Scenario 1: happy path (args and kwargs)");
        assert_eq!(
            rx::Error {
                request_type: 1234,
                request: Id::<SessionScope>::from_raw_value(2345),
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::Integer(1));
                    details
                },
                error: Uri::raw("org.foo.bar.error"),
                arguments: Some(vec![TV::Integer(1), TV::String("asdf".into())]),
                arguments_kw: Some({
                    let mut args = HashMap::new();
                    args.insert("y".into(), TV::Bool(true));
                    args
                }),
            },
            parse_error(&[
                json!(1234),
                json!(2345),
                json!({"x": 1}),
                json!("org.foo.bar.error"),
                json!([1, "asdf"]),
                json!({"y": true})
            ])
            .unwrap()
        );

        println!("Scenario 2: happy path (kwargs only)");
        assert_eq!(
            rx::Error {
                request_type: 1234,
                request: Id::<SessionScope>::from_raw_value(2345),
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::Integer(1));
                    details
                },
                error: Uri::raw("org.foo.bar.error"),
                arguments: Some(vec![]),
                arguments_kw: Some({
                    let mut args = HashMap::new();
                    args.insert("y".into(), TV::Bool(true));
                    args
                }),
            },
            parse_error(&[
                json!(1234),
                json!(2345),
                json!({"x": 1}),
                json!("org.foo.bar.error"),
                json!([]),
                json!({"y": true})
            ])
            .unwrap()
        );

        println!("Scenario 3: happy path (args only)");
        assert_eq!(
            rx::Error {
                request_type: 1234,
                request: Id::<SessionScope>::from_raw_value(2345),
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::Integer(1));
                    details
                },
                error: Uri::raw("org.foo.bar.error"),
                arguments: Some(vec![TV::Integer(1), TV::String("asdf".into())]),
                arguments_kw: None,
            },
            parse_error(&[
                json!(1234),
                json!(2345),
                json!({"x": 1}),
                json!("org.foo.bar.error"),
                json!([1, "asdf"]),
            ])
            .unwrap()
        );

        println!("Scenario 4: fail to parse request type");
        assert!(parse_error(&[
            json!("asdf"),
            json!(1234),
            json!({}),
            json!("org.foo.bar.error")
        ])
        .is_err());

        println!("Scenario 5: fail to parse request ID");
        assert!(parse_error(&[
            json!(1234),
            json!("asdf"),
            json!({}),
            json!("org.foo.bar.error")
        ])
        .is_err());

        println!("Scenario 6: fail to parse details");
        assert!(parse_error(&[
            json!(1234),
            json!(2345),
            json!([]),
            json!("org.foo.bar.error")
        ])
        .is_err());

        println!("Scenario 7: fail to parse error (string)");
        assert!(
            parse_error(&[json!(1234), json!(2345), json!({}), json!([true, 1, false])]).is_err()
        );

        println!("Scenario 8: fail to parse error (uri)");
        assert!(parse_error(&[json!(1234), json!(2345), json!({}), json!("a.#.b")]).is_err());

        println!("Scenario 9: fail to parse arguments");
        assert!(
            parse_error(&[json!(1234), json!(2345), json!({}), json!("a.b"), json!({})]).is_err()
        );

        println!("Scenario 10: fail to parse keyword arguments");
        assert!(parse_error(&[
            json!(1234),
            json!(2345),
            json!({}),
            json!("a.b"),
            json!([]),
            json!("a.b.c.d")
        ])
        .is_err());
    }

    #[test]
    fn parse_goodbye_test() {
        println!("Scenario 0: happy path");
        assert_eq!(
            rx::Goodbye {
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::Integer(1));
                    details.insert("y".into(), TV::Bool(true));
                    details
                },
                reason: Uri::raw("org.foo.bar.closed"),
            },
            parse_goodbye(&[json!({"x": 1, "y": true}), json!("org.foo.bar.closed")]).unwrap()
        );

        println!("Scenario 1: fail to parse details");
        assert!(parse_goodbye(&[json!("foobar"), json!("org.foo.bar.closed")]).is_err());

        println!("Scenario 2: fail to parse reason (string)");
        assert!(parse_goodbye(&[json!({"x": 1, "y": true}), json!(12345)]).is_err());

        println!("Scenario 3: fail to parse reason (uri)");
        assert!(parse_goodbye(&[json!({"x": 1, "y": true}), json!("..")]).is_err());

        println!("Scenario 4: not enough arguments");
        assert!(parse_goodbye(&[json!({"x": 1, "y": true})]).is_err());

        println!("Scenario 5: too many arguments");
        assert!(parse_goodbye(&[
            json!({"x": 1, "y": true}),
            json!("org.foo.bar.closed"),
            json!(12345),
        ])
        .is_err());
    }

    #[cfg(feature = "subscriber")]
    #[test]
    fn parse_subscribed_test() {
        println!("Scenario 0: happy path");
        assert_eq!(
            rx::Subscribed {
                request: Id::<SessionScope>::from_raw_value(12345),
                subscription: Id::<RouterScope>::from_raw_value(23456),
            },
            parse_subscribed(&[json!(12345), json!(23456)]).unwrap()
        );

        println!("Scenario 1: fail to parse request");
        assert!(parse_subscribed(&[json!("foobar"), json!(23456)]).is_err());

        println!("Scenario 2: fail to parse subscription");
        assert!(parse_subscribed(&[json!(12345), json!("foobar")]).is_err());

        println!("Scenario 3: not enough arguments");
        assert!(parse_subscribed(&[json!(12345)]).is_err());

        println!("Scenario 4: too many arguments");
        assert!(parse_subscribed(&[json!(12345), json!(23456), json!(34567)]).is_err());
    }

    #[cfg(feature = "subscriber")]
    #[test]
    fn parse_unsubscribed_test() {
        println!("Scenario 0: happy path");
        assert_eq!(
            rx::Unsubscribed {
                request: Id::<SessionScope>::from_raw_value(12345),
            },
            parse_unsubscribed(&[json!(12345)]).unwrap()
        );

        println!("Scenario 1: fail to parse request");
        assert!(parse_unsubscribed(&[json!("foobar")]).is_err());

        println!("Scenario 2: not enough arguments");
        assert!(parse_unsubscribed(&[]).is_err());

        println!("Scenario 3: too many arguments");
        assert!(parse_unsubscribed(&[json!(12345), json!(23456)]).is_err());
    }

    #[cfg(feature = "subscriber")]
    #[test]
    fn parse_event_test() {
        println!("Scenario 0: happy path (no args)");
        assert_eq!(
            rx::Event {
                subscription: Id::<RouterScope>::from_raw_value(12345),
                publication: Id::<GlobalScope>::from_raw_value(23456),
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::String("foo".into()));
                    details.insert("y".into(), TV::Bool(true));
                    details
                },
                arguments: None,
                arguments_kw: None,
            },
            parse_event(&[json!(12345), json!(23456), json!({"x": "foo", "y": true})]).unwrap()
        );

        println!("Scenario 1: happy path (args only)");
        assert_eq!(
            rx::Event {
                subscription: Id::<RouterScope>::from_raw_value(12345),
                publication: Id::<GlobalScope>::from_raw_value(23456),
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::String("foo".into()));
                    details.insert("y".into(), TV::Bool(true));
                    details
                },
                arguments: Some(vec![TV::Integer(1), TV::String("foobar".into())]),
                arguments_kw: None,
            },
            parse_event(&[
                json!(12345),
                json!(23456),
                json!({"x": "foo", "y": true}),
                json!([1, "foobar"]),
            ])
            .unwrap()
        );

        println!("Scenario 2: happy path (args and kwargs)");
        assert_eq!(
            rx::Event {
                subscription: Id::<RouterScope>::from_raw_value(12345),
                publication: Id::<GlobalScope>::from_raw_value(23456),
                details: {
                    let mut details = HashMap::new();
                    details.insert("x".into(), TV::String("foo".into()));
                    details.insert("y".into(), TV::Bool(true));
                    details
                },
                arguments: Some(vec![TV::Integer(1), TV::String("foobar".into())]),
                arguments_kw: Some({
                    let mut args = HashMap::new();
                    args.insert("a".into(), TV::Integer(1));
                    args.insert("b".into(), TV::Integer(2));
                    args
                }),
            },
            parse_event(&[
                json!(12345),
                json!(23456),
                json!({"x": "foo", "y": true}),
                json!([1, "foobar"]),
                json!({"a": 1, "b": 2}),
            ])
            .unwrap()
        );

        println!("Scenario 3: fail to parse subscription");
        assert!(parse_event(&[json!("foobar"), json!(23456), json!({})]).is_err());

        println!("Scenario 4: fail to parse publication");
        assert!(parse_event(&[json!(12345), json!("foobar"), json!({})]).is_err());

        println!("Scenario 5: fail to parse details");
        assert!(parse_event(&[json!(12345), json!(23456), json!("foobar")]).is_err());

        println!("Scenario 6: fail to parse arguments");
        assert!(
            parse_event(&[json!(12345), json!(23456), json!({}), json!("not a list")]).is_err()
        );

        println!("Scenario 7: fail to parse keyword arguments");
        assert!(parse_event(&[
            json!(12345),
            json!(23456),
            json!({}),
            json!([]),
            json!("not a dict"),
        ])
        .is_err());

        println!("Scenario 8: too many arguments");
        assert!(parse_event(&[
            json!(12345),
            json!(23456),
            json!({}),
            json!([]),
            json!({}),
            json!("foobar"),
        ])
        .is_err());

        println!("Scenario 9: not enough arguments");
        assert!(parse_event(&[json!(12345), json!(23456)]).is_err());
    }
}
