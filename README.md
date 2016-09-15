# ping-rs - a UDP ASCII ping client with userspace networking

ping-rs is a UDP ASCII ping client which makes use of [librips](https://github.com/faern/librips) for userspace networking. It provides a basic client with stats to help with benchmarking

## Usage

To use `ping-rs`, first clone the repo:

With stable rust, just build and run (note: you must change the parameters to reflect your environment):
```shell
git clone https://github.com/brayniac/ping-rs
cargo build --release
sudo ./target/release/ping-rs --ip 10.138.0.2/32 --gateway 10.138.0.1 eth0 10.138.0.3:12221
```

With nightly rust, you may use the 'asm' feature to provide lower-cost timestamping of events:
```shell
git clone https://github.com/brayniac/ping-rs
cargo build --release --features asm
sudo ./target/release/ping-rs --ip 10.138.0.2/32 --gateway 10.138.0.1 eth0 10.138.0.3:12221
```

Upon completion, a 'ok_waterfall.png' will be created with the full latency distribution available to view. A 'ok_trace.txt' will have the trace file for the run (a series of histograms capturing the latency values). The rate metrics will be output to stdout.

## Features

* simple ASCII ping client
* userspace UDP implementation
* stats to measure performance

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
