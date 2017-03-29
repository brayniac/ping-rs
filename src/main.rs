#[macro_use]
extern crate log;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate lazy_static;
extern crate ipnetwork;
extern crate pnet;
extern crate rips;
extern crate smoltcp;
extern crate tic;
extern crate time;

use std::fmt;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::process;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread;

use ipnetwork::Ipv4Network;
use pnet::datalink::{self, NetworkInterface};
use rips::udp::UdpSocket;
use tic::{Clocksource, Interest, Receiver, Sample, Sender};

mod logging;
use logging::set_log_level;

lazy_static! {
    static ref DEFAULT_ROUTE: Ipv4Network = "0.0.0.0/0".parse().unwrap();
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
            Metric::Ok => write!(f, "response_ok"),
        }
    }
}

fn main() {
    set_log_level(2);
    let args = ArgumentParser::new();

    let (_, iface) = args.get_iface();
    let src_net = args.get_src_net();
    let gateway = args.get_gw();
    let channel = args.create_channel();
    let duration = args.get_duration();
    let windows = args.get_windows();
    let stats_qlen = args.get_stats_qlen();
    let dst = args.get_dst();
    let threads = args.get_threads();
    let noop = args.get_noop();
    let stdnet = args.get_stdnet();

    let mut stack = rips::NetworkStack::new();
    stack.add_interface(iface.clone(), channel).unwrap();
    stack.add_ipv4(&iface, src_net).unwrap();
    {
        let routing_table = stack.routing_table();
        routing_table.add_route(*DEFAULT_ROUTE, Some(gateway), iface);
    }

    let stack = Arc::new(Mutex::new(stack));

    // initialize a tic::Receiver to ingest stats
    let mut receiver = Receiver::configure()
        .windows(windows)
        .duration(duration)
        .capacity(stats_qlen)
        .http_listen("0.0.0.0:42024".to_owned())
        .build();

    receiver.add_interest(Interest::Waterfall(Metric::Ok, "ok_waterfall.png".to_owned()));
    receiver.add_interest(Interest::Trace(Metric::Ok, "ok_trace.txt".to_owned()));
    receiver.add_interest(Interest::Percentile(Metric::Ok));
    receiver.add_interest(Interest::Count(Metric::Ok));

    for _ in 0..threads {
        let sender = receiver.get_sender();
        let clocksource = receiver.get_clocksource();
        let src = SocketAddr::V4(SocketAddrV4::new(src_net.ip(), 0));
        let dst = dst;
        if noop {
            thread::spawn(move || {
                handle_noop(clocksource, sender);
            });
        } else if stdnet {
            let socket = std::net::UdpSocket::bind(src).unwrap();
            thread::spawn(move || {
                handle_stdnet(socket, dst, clocksource, sender);
            });
        } else {
            let socket = UdpSocket::bind(stack.clone(), src).unwrap();
            thread::spawn(move || {
                handle_rips(socket, dst, clocksource, sender);
            });
        }
    }

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
        info!("rate: {} rps", r);
        info!("latency: p50: {} ns p90: {} ns p99: {} ns p999: {} ns p9999: {} ns",
                    m.get_combined_percentile(
                        tic::Percentile("p50".to_owned(), 50.0)).unwrap_or(&0),
                    m.get_combined_percentile(
                        tic::Percentile("p90".to_owned(), 90.0)).unwrap_or(&0),
                    m.get_combined_percentile(
                        tic::Percentile("p99".to_owned(), 99.0)).unwrap_or(&0),
                    m.get_combined_percentile(
                        tic::Percentile("p999".to_owned(), 99.9)).unwrap_or(&0),
                    m.get_combined_percentile(
                        tic::Percentile("p9999".to_owned(), 99.99)).unwrap_or(&0),
                );
    }
    info!("saving files...");
    receiver.save_files();
    info!("complete");
}

fn handle_rips(mut socket: UdpSocket,
               dst: SocketAddr,
               clocksource: Clocksource,
               stats: Sender<Metric>) {
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

fn handle_stdnet(socket: std::net::UdpSocket,
                 dst: SocketAddr,
                 clocksource: Clocksource,
                 stats: Sender<Metric>) {
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

fn handle_noop(clocksource: Clocksource, stats: Sender<Metric>) {
    loop {
        let t0 = clocksource.counter();
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
        for iface in datalink::interfaces() {
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
            match src_net.parse() {
                Ok(src_net) => src_net,
                Err(_) => self.print_error("Invalid CIDR"),
            }
        } else {
            let (iface, _) = self.get_iface();
            if let Some(ips) = iface.ips.as_ref() {
                for ip in ips {
                    if let IpAddr::V4(ip) = *ip {
                        return Ipv4Network::new(ip, 24).unwrap();
                    }
                }
            }
            self.print_error("No IPv4 to use on given interface");
        }
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

    pub fn get_stats_qlen(&self) -> usize {
        let matches = &self.matches;
        match value_t!(matches, "stats-qlen", usize) {
            Ok(v) => v,
            Err(e) => self.print_error(&format!("Invalid duration param. {}", e)),
        }
    }

    pub fn get_threads(&self) -> usize {
        let matches = &self.matches;
        match value_t!(matches, "threads", usize) {
            Ok(v) => v,
            Err(e) => self.print_error(&format!("Invalid duration param. {}", e)),
        }
    }

    pub fn get_noop(&self) -> bool {
        let matches = &self.matches;
        matches.is_present("noop")
    }

    pub fn get_stdnet(&self) -> bool {
        let matches = &self.matches;
        matches.is_present("stdnet")
    }

    pub fn create_channel(&self) -> rips::EthernetChannel {
        let (iface, _) = self.get_iface();
        let mut config = datalink::Config::default();
        config.write_buffer_size = 1024 * 64;
        config.read_buffer_size = 1024 * 64;
        match datalink::channel(&iface, config) {
            Ok(datalink::Channel::Ethernet(tx, rx)) => rips::EthernetChannel(tx, rx),
            _ => self.print_error(&format!("Unable to open network channel on {}", iface.name)),
        }
    }

    fn create_app() -> clap::App<'static, 'static> {
        let src_net_arg = clap::Arg::with_name("src_net")
            .long("ip")
            .value_name("CIDR")
            .help("Local IP and prefix to send from, in CIDR format. Will default to first IP on \
                   given iface and prefix 24.")
            .takes_value(true);
        let gw = clap::Arg::with_name("gw")
            .long("gateway")
            .value_name("IP")
            .help("The default gateway to use if the destination is not on the local network. \
                   Must be inside the network given to --ip. Defaults to the first address in \
                   the network given to --ip")
            .takes_value(true);
        let iface_arg = clap::Arg::with_name("iface")
            .help("Network interface to use")
            .required(true)
            .index(1);
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
        let stats_qlen = clap::Arg::with_name("stats-qlen")
            .long("stats-qlen")
            .value_name("COUNT")
            .help("Capacity of the stats queue")
            .takes_value(true)
            .default_value("1024");
        let threads = clap::Arg::with_name("threads")
            .long("threads")
            .value_name("COUNT")
            .help("Number of client threads to use")
            .takes_value(true)
            .default_value("1");
        let noop = clap::Arg::with_name("noop")
            .long("noop")
            .help("no-op validation of stats")
            .takes_value(false);
        let stdnet = clap::Arg::with_name("stdnet")
            .long("stdnet")
            .help("use std::net::UdpSocket")
            .takes_value(false);

        clap::App::new("UDP Ping Client")
            .version(crate_version!())
            .author(crate_authors!())
            .about("A simple UDP ping client with a userspace network stack")
            .arg(src_net_arg)
            .arg(gw)
            .arg(windows)
            .arg(duration)
            .arg(iface_arg)
            .arg(dst_arg)
            .arg(stats_qlen)
            .arg(threads)
            .arg(noop)
            .arg(stdnet)
    }

    fn print_error(&self, error: &str) -> ! {
        eprintln!("ERROR: {}\n", error);
        self.app.write_help(&mut ::std::io::stderr()).unwrap();
        eprintln!("");
        process::exit(1);
    }
}
