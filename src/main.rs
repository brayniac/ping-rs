#[macro_use] extern crate clap;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;
extern crate ipnetwork;
extern crate pad;
extern crate pnet;
extern crate rips;
extern crate tic;
extern crate time;

use std::io::{Write};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::process;
use std::sync::{Arc, Mutex};
use std::str::FromStr;
use std::thread;

use ipnetwork::Ipv4Network;
use pnet::datalink::{self, NetworkInterface};
use rips::udp::UdpSocket;
use tic::{Clocksource, Interest, Receiver, Sample, Sender};


lazy_static! {
    static ref DEFAULT_ROUTE: Ipv4Network = Ipv4Network::from_cidr("0.0.0.0/0").unwrap();
}

macro_rules! eprintln {
    ($($arg:tt)*) => (
        match writeln!(&mut ::std::io::stderr(), $($arg)* ) {
            Ok(_) => {},
            Err(x) => panic!("Unable to write to stderr: {}", x),
        }
    )
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Metric {
    Ok,
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Metric::Ok => write!(f, "ok"),
        }
    }
}

fn main() {
    let args = ArgumentParser::new();

    let (_, iface) = args.get_iface();
    let src_net = args.get_src_net();
    let gateway = args.get_gw();
    let src_port = args.get_src_port();
    let channel = args.create_channel();
    let duration = args.get_duration();
    let windows = args.get_windows();
    let src = SocketAddr::V4(SocketAddrV4::new(src_net.ip(), src_port));
    let dst = args.get_dst();

    let mut stack = rips::NetworkStack::new();
    stack.add_interface(iface.clone(), channel).unwrap();
    stack.add_ipv4(&iface, src_net).unwrap();
    {
        let routing_table = stack.routing_table();
        routing_table.add_route(*DEFAULT_ROUTE, Some(gateway), iface);
    }

    let stack = Arc::new(Mutex::new(stack));
    let socket = UdpSocket::bind(stack, src).unwrap();

    // initialize a tic::Receiver to injest stats
    let mut receiver = Receiver::configure()
        .windows(windows)
        .duration(duration)
        .capacity(100_000)
        .http_listen("localhost:42024".to_owned())
        .build();

    receiver.add_interest(Interest::Waterfall(Metric::Ok, "ok_waterfall.png".to_owned()));
    receiver.add_interest(Interest::Trace(Metric::Ok, "ok_trace.txt".to_owned()));
    receiver.add_interest(Interest::Count(Metric::Ok));

    let sender = receiver.get_sender();
    let clocksource = receiver.get_clocksource();

    thread::spawn(move || {
            handle(socket, dst, clocksource, sender);
        });
    
    let cs = receiver.get_clocksource();

    let mut total = 0;

    for _ in 0..windows {
        let t0 = cs.time();
        receiver.run_once();
        let t1 = cs.time();
        let m = receiver.clone_meters();
        let mut c = 0;
        if let Some(t) = m.get_combined_count() {
            c = *t - total;
            total = *t;
        }
        let r = c as f64 / ((t1 - t0) as f64 / 1_000_000_000.0);
        println!("rate: {} samples per second", r);
    }
    println!("saving files...");
    receiver.save_files();
    println!("complete");
}

fn handle(mut socket: UdpSocket, dst: SocketAddr, clocksource: Clocksource, stats: Sender<Metric>) {
    let request = "PING\r\n".to_owned().into_bytes();
    let mut buffer = vec![0; 1024*2];
    loop {
        let t0 = clocksource.counter();
        let _ = socket.send_to(&request, dst);
        let (_, _) = socket.recv_from(&mut buffer).expect("Unable to read from socket");
        let t1 = clocksource.counter();
        let _ = stats.send(Sample::new(t0, t1, Metric::Ok));
    }
}

struct ArgumentParser {
    app: clap::App<'static, 'static>,
    matches: clap::ArgMatches<'static>,
}

impl ArgumentParser {
    pub fn new() -> ArgumentParser {
        let app = Self::create_app();
        let matches = app.clone().get_matches();
        ArgumentParser {
            app: app,
            matches: matches,
        }
    }

    pub fn get_iface(&self) -> (NetworkInterface, rips::Interface) {
        let iface_name = self.matches.value_of("iface").unwrap();
        for iface in datalink::interfaces().into_iter() {
            if iface.name == iface_name {
                if let Ok(rips_iface) = rips::convert_interface(&iface) {
                    return (iface, rips_iface);
                } else {
                    self.print_error(&format!("Interface {} can't be used with rips", iface_name));
                }
            }
        }
        self.print_error(&format!("Found no interface named {}", iface_name));
    }

    pub fn get_src_net(&self) -> Ipv4Network {
        if let Some(src_net) = self.matches.value_of("src_net") {
            match Ipv4Network::from_cidr(src_net) {
                Ok(src_net) => src_net,
                Err(_) => self.print_error("Invalid CIDR"),
            }
        } else {
            let (iface, _) = self.get_iface();
            if let Some(ips) = iface.ips.as_ref() {
                for ip in ips {
                    if let &IpAddr::V4(ip) = ip {
                        return Ipv4Network::new(ip, 24).unwrap();
                    }
                }
            }
            self.print_error("No IPv4 to use on given interface");
        }
    }

    pub fn get_src_port(&self) -> u16 {
        let matches = &self.matches;
        value_t!(matches, "src_port", u16).unwrap()
    }

    pub fn get_gw(&self) -> Ipv4Addr {
        if let Some(gw_str) = self.matches.value_of("gw") {
            if let Ok(gw) = Ipv4Addr::from_str(gw_str) {
                gw
            } else {
                self.print_error("Unable to parse gateway ip");
            }
        } else {
            let src_net = self.get_src_net();
            if let Some(gw) = src_net.nth(1) {
                gw
            } else {
                self.print_error(&format!("Could not guess a default gateway inside {}", src_net));
            }
        }
    }

    pub fn get_dst(&self) -> SocketAddr {
        let matches = &self.matches;
        match value_t!(matches, "target", SocketAddr) {
            Ok(dst) => dst,
            Err(e) => self.print_error(&format!("Invalid target. {}", e)),
        }
    }

    pub fn get_windows(&self) -> usize {
        let matches = &self.matches;
        match value_t!(matches, "windows", usize) {
            Ok(v) => v,
            Err(e) => self.print_error(&format!("Invalid windows param. {}", e)),
        }
    }

    pub fn get_duration(&self) -> usize {
        let matches = &self.matches;
        match value_t!(matches, "duration", usize) {
            Ok(v) => v,
            Err(e) => self.print_error(&format!("Invalid duration param. {}", e)),
        }
    }

    pub fn create_channel(&self) -> rips::EthernetChannel {
        let (iface, _) = self.get_iface();
        let mut config = datalink::Config::default();
        config.write_buffer_size = 1024*64;
        config.read_buffer_size = 1024*64;
        match datalink::channel(&iface, config) {
            Ok(datalink::Channel::Ethernet(tx, rx)) => rips::EthernetChannel(tx, rx),
            _ => self.print_error(&format!("Unable to open network channel on {}", iface.name)),
        }
    }

    fn create_app() -> clap::App<'static, 'static> {
        let src_net_arg = clap::Arg::with_name("src_net")
            .long("ip")
            .value_name("CIDR")
            .help("Local IP and prefix to send from, in CIDR format. Will default to first IP on given iface and prefix 24.")
            .takes_value(true);
        let gw = clap::Arg::with_name("gw")
            .long("gateway")
            .value_name("IP")
            .help("The default gateway to use if the destination is not on the local network. Must be inside the network given to --ip. Defaults to the first address in the network given to --ip")
            .takes_value(true);
        let iface_arg = clap::Arg::with_name("iface")
            .help("Network interface to use")
            .required(true)
            .index(1);
        let src_port_arg = clap::Arg::with_name("src_port")
            .long("sport")
            .value_name("PORT")
            .help("Local port to bind to and send from.")
            .default_value("12321");
        let dst_arg = clap::Arg::with_name("target")
            .help("Target to connect to. Given as <ip>:<port>")
            .required(true)
            .index(2);
        let windows = clap::Arg::with_name("windows")
            .long("windows")
            .value_name("COUNT")
            .help("Number of integration windows per run")
            .takes_value(true)
            .default_value("5");
        let duration = clap::Arg::with_name("duration")
            .long("duration")
            .value_name("SECONDS")
            .help("Seconds per integration window")
            .takes_value(true)
            .default_value("60");

        let app = clap::App::new("UDP Ping Client")
            .version(crate_version!())
            .author(crate_authors!())
            .about("A simple UDP ping client with a userspace network stack")
            .arg(src_net_arg)
            .arg(src_port_arg)
            .arg(gw)
            .arg(windows)
            .arg(duration)
            .arg(iface_arg)
            .arg(dst_arg);

        app
    }

    fn print_error(&self, error: &str) -> ! {
        eprintln!("ERROR: {}\n", error);
        self.app.write_help(&mut ::std::io::stderr()).unwrap();
        eprintln!("");
        process::exit(1);
    }
}
