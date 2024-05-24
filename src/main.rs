use kissvpn::cipher::Cipher;
use kissvpn::constants::VPN_MTU;
use kissvpn::engine;
use kissvpn::transport::fakedns::{FakednsClientTransport, FakednsServerTransport};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server_addr = std::env::args().nth(1).unwrap();

    let mut tun_config = tun::Configuration::default();
    tun_config
        .mtu(VPN_MTU as i32)
        .name("tun_kiss")
        .up();

    #[cfg(target_os = "linux")]
    tun_config.platform(|config| {
        config.packet_information(false);
    });

    let cipher = Cipher::new("key");

    if server_addr == "server" {
        tun_config
            .address((192, 168, 99, 1))
            .netmask((255, 255, 255, 0))
            .destination((192, 168, 99, 2));
        let tun_dev = tun::create_as_async(&tun_config)?;
        let transport = FakednsServerTransport::create("0.0.0.0:9000").await?;
        engine::run(tun_dev, transport, cipher).await?;
    } else {
        tun_config
            .address((192, 168, 99, 2))
            .netmask((255, 255, 255, 0))
            .destination((192, 168, 99, 1));
        let tun_dev = tun::create_as_async(&tun_config)?;
        let transport = FakednsClientTransport::create("0.0.0.0:0", server_addr + ":9000").await?;
        engine::run(tun_dev, transport, cipher).await?;
    }

    Ok(())
}
