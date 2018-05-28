
extern crate failure;
extern crate futures;
extern crate wamp_proto;

use failure::Error;
use futures::prelude::*;
use wamp_proto::{Transport, proto};

struct MockTransport {
    closed: bool,
    sent_messages: Vec<proto::TxMessage>,
    flushed_messages: Vec<proto::TxMessage>,
}
impl Sink for MockTransport {
    type SinkItem = proto::TxMessage;
    type SinkError = Error;

    fn start_send(&mut self, item: proto::TxMessage) -> Result<(), Error> {
        if self.closed {
            Err(format_err!("already closed"))
        } else {
            self.sent_messages.push(item);
            Ok(())
        }
    }

    fn poll_flush(&mut self, cx: &mut Context) -> Result<Async<()>, Error> {
        if self.closed {
            Err(format_err!("already closed"))
        } else {
            self.flushed_messages.append(self.sent_messages);
            Ok(Async::Ready)
        }
    }

    fn poll_close(&mut self, cx: &mut Context) -> Result<Async<()>, Error> {
        if self.closed {
            Err(format_err!("already closed"))
        } else {
            self.flushed_messages.append(self.sent_messages);
            self.closed = true;
            Ok(Async::Ready)
        }
    }
    
}
impl Transport for MockTransport {
    
}