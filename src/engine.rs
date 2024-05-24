use std::sync::Arc;

use anyhow::Result;
use bytes::BytesMut;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::task;

use crate::constants::UDP_MTU;
use crate::transport::Transport;
use crate::cipher::Cipher;


async fn run_read_tun(mut tun_reader: ReadHalf<tun::AsyncDevice>,
                      transport: &impl Transport,
                      cipher: Cipher) -> Result<()> {
    loop {
        let mut buf = BytesMut::with_capacity(UDP_MTU);
        tun_reader.read_buf(&mut buf).await?;
        cipher.encrypt(&mut buf)?;
        if transport.ready_to_send() {
            transport.send(buf.into()).await?;
        }
    }
}

async fn run_write_tun(mut tun_writer: WriteHalf<tun::AsyncDevice>,
                       transport: &impl Transport,
                       cipher: Cipher) -> Result<()> {
    loop {
        let mut buf = transport.receive().await?;
        if cipher.decrypt(&mut buf).is_ok() {
            transport.mark_last_received_valid();
            tun_writer.write(&buf).await?;
        }
    }
}

pub async fn run(tun_dev: tun::AsyncDevice,
                 transport: impl Transport + 'static,
                 cipher: Cipher) -> Result<()> {
    let transport = Arc::new(transport);
    let (tun_reader, tun_writer) = io::split(tun_dev);

    // TODO: keepalive transport

    let read_tun_handler = {
        let transport = transport.clone();
        let cipher = cipher.clone();
        task::spawn(async move {
            run_read_tun(tun_reader, transport.as_ref(), cipher).await.unwrap();
        })
    };
    let write_tun_handler = task::spawn(async move {
        run_write_tun(tun_writer, transport.as_ref(), cipher).await.unwrap();
    });

    tokio::join!(read_tun_handler, write_tun_handler);

    Ok(())
}
