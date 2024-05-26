use std::thread;

use anyhow::Result;
use bytes::BytesMut;

use crate::constants::UDP_MTU;
use crate::transport::Transport;
use crate::cipher::Cipher;


fn run_read_tun(tun_reader: &tun_tap::Iface,
                transport: &impl Transport,
                cipher: Cipher) -> Result<()> {
    loop {
        let mut buf = vec![0u8; UDP_MTU];
        let buf_len = tun_reader.recv(&mut buf)?;
        let mut buf = BytesMut::from(&buf[0..buf_len]);  // TODO
        cipher.encrypt(&mut buf)?;
        if transport.ready_to_send() {
            transport.send(buf.into())?;
        }
    }
}

fn run_write_tun(tun_writer: &tun_tap::Iface,
                 transport: &impl Transport,
                 cipher: Cipher) -> Result<()> {
    loop {
        let mut buf = transport.receive()?;
        if cipher.decrypt(&mut buf).is_ok() {
            transport.mark_last_received_valid();
            tun_writer.send(&buf)?;
        }
    }
}

pub fn run(tun_dev: tun_tap::Iface,
           transport: impl Transport + 'static,
           cipher: Cipher) -> Result<()> {
    thread::scope(|s| {
        {
            let cipher = cipher.clone();
            s.spawn(|| {
                run_read_tun(&tun_dev, &transport, cipher).unwrap();
            });
        }
        s.spawn(|| {
            run_write_tun(&tun_dev, &transport, cipher).unwrap();
        });
    });

    Ok(())
}
