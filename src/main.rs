use std::process::Command;

use kissvpn::cipher::Cipher;
use kissvpn::constants::VPN_MTU;
use kissvpn::engine;
use kissvpn::transport::fakedns::{FakednsClientTransport, FakednsServerTransport};


fn cmd(cmd: &str, args: &[&str]) {
    let ecode = Command::new("ip")
        .args(args)
        .spawn()
        .unwrap()
        .wait()
        .unwrap();
    assert!(ecode.success(), "Failed to execte {}", cmd);
}

fn main() -> anyhow::Result<()> {
    let server_addr = std::env::args().nth(1).unwrap();

    let tun_dev = tun_tap::Iface::without_packet_info("tun_kiss", tun_tap::Mode::Tun)?;
    let cipher = Cipher::new("key");

    if server_addr == "server" {
        cmd("ip", &["addr", "add", "dev", "tun_kiss", "192.168.99.1/24", "peer", "192.168.99.2"]);
        cmd("ip", &["link", "set", "tun_kiss", "mtu", &format!("{}", VPN_MTU), "up"]);
        let transport = FakednsServerTransport::create("0.0.0.0:9000")?;
        engine::run(tun_dev, transport, cipher)
    } else {
        cmd("ip", &["addr", "add", "dev", "tun_kiss", "192.168.99.2/24", "peer", "192.168.99.1"]);
        cmd("ip", &["link", "set", "tun_kiss", "mtu", &format!("{}", VPN_MTU), "up"]);
        let transport = FakednsClientTransport::create("0.0.0.0:0", server_addr + ":9000")?;
        engine::run(tun_dev, transport, cipher)
    }
}
