// Shared test utilities for faber-lsp. Only compiled in test mode.
#![cfg(test)]

use crossbeam_channel::{Receiver, Sender, unbounded};
use std::io::{self, Read, Write};

pub struct ChanReader(pub Receiver<u8>);
pub struct ChanWriter(pub Sender<u8>);

impl Read for ChanReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self.0.recv() {
            Ok(b) => {
                buf[0] = b;
                let mut n = 1;
                while n < buf.len() {
                    match self.0.try_recv() {
                        Ok(b) => {
                            buf[n] = b;
                            n += 1;
                        }
                        Err(_) => break,
                    }
                }
                Ok(n)
            }
            Err(_) => Ok(0), // EOF
        }
    }
}

impl Write for ChanWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &b in buf {
            self.0
                .send(b)
                .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn byte_pipe() -> (ChanWriter, ChanReader) {
    let (tx, rx) = unbounded::<u8>();
    (ChanWriter(tx), ChanReader(rx))
}
