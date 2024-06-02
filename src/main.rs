use std::io::Read;
use std::process::Command;

use kissvpn::cipher::Cipher;
use kissvpn::constants::VPN_MTU;
use kissvpn::engine;
use kissvpn::transport::fakedns::{FakednsClientTransport, FakednsServerTransport};
use kissvpn::transport::udp::UdpClientTransportOptions;
use kissvpn::tun::TunDevice;
use log::info;
use clap::{Parser, Subcommand};


#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, help="Key string. If key starts with @, then read from the file")]
    key: String,

    #[arg(short, long, help="Run this script to configure interface. Arg: IFACE")]
    up_script: Option<String>,

    #[command(subcommand)]
    action: Action,

    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
}

#[derive(Subcommand, Debug)]
enum Action {
    Serve {
        bind: String,
    },
    Connect {
        remote: String,

        #[arg(long, default_value_t = 10)]
        num_sockets: i32,
    },
}

fn run_cmd(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    info!("Running `{} {}'", cmd, args.join(" "));
    let ret = Command::new(cmd).args(args)
        .spawn()?.wait()?;
    if !ret.success() {
        anyhow::bail!("Command failed with return code {}", ret.code().unwrap());
    }
    Ok(())
}


fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    simple_logger::SimpleLogger::new()
        .with_level(args.verbose.log_level_filter())
        .init()?;

    // exit the process if any thread panic
    let original_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        original_panic_hook(info);
        std::process::exit(1);
    }));

    let tun_dev = TunDevice::create("tun%d")?;
    let tun_name = tun_dev.name();
    info!("Tun device created: {tun_name}");

    if let Some(up_script) = &args.up_script {
        run_cmd(&up_script, &[tun_name])?;
    }
    run_cmd("ip", &["link", "set", tun_name, "mtu", &format!("{}", VPN_MTU), "up"])?;

    let key = if args.key.starts_with("@") {
        let mut f = std::fs::File::open(&args.key[1..])?;
        let mut result = String::new();
        f.read_to_string(&mut result)?;
        result.trim().into()
    } else {
        args.key.clone()
    };
    let cipher = Cipher::new(&key);

    match args.action {
        Action::Serve { bind } => {
            let transport = FakednsServerTransport::create(&bind)?;
            engine::run(tun_dev, transport, cipher)
        },
        Action::Connect { remote, num_sockets } => {
            let transport = FakednsClientTransport::create(
                &remote,
                UdpClientTransportOptions {
                    max_send_sockets: num_sockets as usize,
                    ..Default::default()
                })?;
            engine::run(tun_dev, transport, cipher)
        },
    }
}
