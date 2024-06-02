use std::io::{Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::{thread, time};

use anyhow::Result;
use bytes::BytesMut;
use log::trace;

use crate::constants::UDP_MTU;
use crate::transport::Transport;
use crate::cipher::Cipher;
use crate::tun::TunDevice;


fn spawn_loop<'scope, F>(scope: &'scope thread::Scope<'scope, '_>, mut f: F)
where F: FnMut() -> anyhow::Result<()> + Send + 'scope {
    scope.spawn(move || {
        loop {
            f().unwrap();
        }
    });
}

const CHANNEL_SIZE: usize = 64;
const KEEPALIVE_INTERVAL: time::Duration = time::Duration::from_secs(60);

pub fn run(tun: TunDevice,
           transport: impl Transport + 'static,
           cipher: Cipher) -> Result<()> {
    let (tun2transport_sender, tun2transport_receiver) = mpsc::sync_channel::<BytesMut>(CHANNEL_SIZE);
    let (transport2tun_sender, transport2tun_receiver) = mpsc::sync_channel::<BytesMut>(CHANNEL_SIZE);

    // the timestamp when last tun->transport packet happen
    // used for scheduling keepalive packet
    let last_tun_read = Arc::new(Mutex::new(time::Instant::now() - KEEPALIVE_INTERVAL * 2));

    thread::scope(|s| {
        // read from tun
        let mut tun_ = &tun;
        let last_tun_read_ = last_tun_read.clone();
        let tun2transport_sender_ = tun2transport_sender.clone();
        spawn_loop(s, move || {
            let mut buf = BytesMut::zeroed(UDP_MTU);
            let buf_len = tun_.read(&mut buf)?;
            buf.truncate(buf_len);
            tun2transport_sender_.send(buf)?;
            *last_tun_read_.lock().unwrap() = time::Instant::now();
            Ok(())
        });

        // send to transport
        let transport_ = &transport;
        let cipher_ = cipher.clone();
        spawn_loop(s, move || {
            let mut buf = tun2transport_receiver.recv()?;
            cipher_.encrypt(&mut buf)?;
            if transport_.ready_to_send() {
                transport_.send(buf)?;
            }
            Ok(())
        });

        // receive from transport
        let transport_ = &transport;
        let cipher_ = cipher.clone();
        spawn_loop(s, move || {
            let mut buf = transport_.receive()?;
            if cipher_.decrypt(&mut buf).is_ok() {
                transport_.mark_last_received_valid();
                if !buf.is_empty() {  // empty is for keepalive
                    transport2tun_sender.send(buf)?;
                }
            } else {
                trace!("Received invalid packet (unable to decrypt)");
            }
            Ok(())
        });

        // write to tun
        let mut tun_ = &tun;
        spawn_loop(s, move || {
            let buf = transport2tun_receiver.recv()?;
            tun_.write(&buf)?;
            Ok(())
        });

        if transport.needs_keepalive() {
            spawn_loop(s, move || {
                let mut last_tun_read_v = *last_tun_read.lock().unwrap();
                let now = time::Instant::now();
                if now > last_tun_read_v + KEEPALIVE_INTERVAL {
                    trace!("Sending keepalive packet");
                    tun2transport_sender.send(BytesMut::new())?;
                    last_tun_read_v = now;
                    *last_tun_read.lock().unwrap() = now;
                }
                thread::sleep(last_tun_read_v + KEEPALIVE_INTERVAL - now);
                Ok(())
            });
        }
    });

    Ok(())
}
