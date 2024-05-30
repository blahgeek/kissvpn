use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;

use anyhow::Result;
use bytes::BytesMut;

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

pub fn run(tun: TunDevice,
           transport: impl Transport + 'static,
           cipher: Cipher) -> Result<()> {
    let (tun2transport_sender, tun2transport_receiver) = mpsc::sync_channel::<BytesMut>(CHANNEL_SIZE);
    let (transport2tun_sender, transport2tun_receiver) = mpsc::sync_channel::<BytesMut>(CHANNEL_SIZE);

    thread::scope(|s| {
        // read from tun
        let mut tun_ = &tun;
        spawn_loop(s, move || {
            let mut buf = BytesMut::zeroed(UDP_MTU);
            let buf_len = tun_.read(&mut buf)?;
            buf.truncate(buf_len);
            tun2transport_sender.send(buf)?;
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
                transport2tun_sender.send(buf)?;
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
    });

    Ok(())
}
