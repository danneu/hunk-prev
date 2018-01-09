#![feature(ip_constructors)]
#![feature(libc)]

extern crate hunk;

extern crate clap;
extern crate colored;
extern crate futures_cpupool;
extern crate hyper;
extern crate libc;
extern crate toml;

use clap::{App, Arg};
use colored::Colorize;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::str::FromStr;
use std::fs::File;
use std::io::Read;

use hunk::HttpService;
use hunk::options;

/// Leaks a given owned object, returning a reference with the static lifetime.
/// This can save dealing with reference-counting, lazy statics, or mutexes.
fn leak<T: ?Sized>(b: Box<T>) -> &'static T {
    unsafe {
        let r = ::std::mem::transmute(&*b);
        ::std::mem::forget(b);
        r
    }
}

fn is_tty() -> bool {
    unsafe { libc::isatty(libc::STDOUT_FILENO as i32) != 0 }
}

fn main() {
    let config_path = PathBuf::from("Hunk.toml");

    let config = if config_path.exists() && config_path.is_file() {
        File::open(config_path)
            .map_err(|err| {
                eprintln!("Could not open config file: {}", err);
                std::process::exit(1);
            })
            .and_then(|mut f| {
                let mut text = String::new();
                f.read_to_string(&mut text).map(|_| text)
            })
            .and_then(|text| {
                hunk::config::parse(text.as_ref()).map_err(|err| {
                    eprintln!("Could not parse config: {}", err);
                    std::process::exit(1);
                })
            })
            .unwrap_or_else(|err| {
                eprintln!("Could not parse config: {}", err);
                std::process::exit(1);
            })
    } else {
        hunk::config::Config::default()
    };

    let matches = App::new("Hunk")
        .about("a static-asset server")
        .arg(
            Arg::with_name("FOLDER")
                .help("the folder to serve")
                .index(1),
        )
        .arg(
            Arg::with_name("port")
                .short("p")
                .long("port")
                .help("the port to bind to")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("host")
                .short("h")
                .long("host")
                .help("the host to bind to")
                .takes_value(true),
        )
        .get_matches();

    let root = PathBuf::from(
        config
            .server
            .root
            .clone()
            .or_else(|| matches.value_of("FOLDER").map(String::from))
            .unwrap_or_else(|| ".".to_string()),
    );

    let port = config
        .server
        .port
        .or_else(|| {
            matches
                .value_of("port")
                .and_then(|s| if s.is_empty() { None } else { Some(s) })
                .and_then(|s| {
                    s.parse::<u16>()
                        .map_err(|err| {
                            eprintln!("Could not parse port: {}", err);
                            ::std::process::exit(1)
                        })
                        .ok()
                })
        })
        .unwrap_or(hunk::config::DEFAULT_PORT);

    let host = config
        .server
        .host
        .or_else(|| {
            matches
                .value_of("host")
                .and_then(|s| if s.is_empty() { None } else { Some(s) })
                .and_then(|s| {
                    Ipv4Addr::from_str(s)
                        .map_err(|err| {
                            eprintln!("Could not parse host: {}", err);
                            ::std::process::exit(1)
                        })
                        .ok()
                })
        })
        .unwrap_or_else(Ipv4Addr::localhost);

    let root = root.canonicalize().unwrap_or_else(|_| {
        eprintln!("Root path not found");
        ::std::process::exit(1);
    });

    if !root.is_dir() {
        eprintln!("Root path must be a directory");
        ::std::process::exit(1);
    }

    let opts = hunk::options::Options::new(config.clone()).unwrap_or_else(|err| {
        eprintln!("{}", err);
        ::std::process::exit(1);
    });

    let ctx = leak(Box::new(hunk::Context {
        root: root.clone(),
        pool: ::futures_cpupool::Builder::new()
            .name_prefix("hunk-thread-")
            .pool_size(1)
            .create(),
        opts: opts.clone(),
    }));

    let addr = SocketAddr::new(IpAddr::V4(host), port);
    let server = hyper::server::Http::new()
        .bind(&addr, move || Ok(HttpService::new(ctx)))
        .unwrap();

    if is_tty() {
        println!();
        println!(
            "{} {}",
            "[hunk]".bright_white().bold(),
            "listening".bright_green().bold()
        );
        println!("folder:  {}", root.to_str().unwrap().bright_white().bold());
        println!(
            "address: {}{}",
            "http://".bright_white(),
            server
                .local_addr()
                .unwrap()
                .to_string()
                .bright_white()
                .bold()
        );
        println!(
            "- gzip: {}",
            match config.gzip.as_ref() {
                None => "off".red().bold().to_string(),
                Some(opts) => {
                    let mut s = format!("{}", "on".green().bold());
                    s.push(' ');
                    s.push_str(format!("level={}/9", opts.level.to_string().bold()).as_ref());
                    s.push(' ');
                    s.push_str(format!("threshold={}", opts.threshold.to_string().bold()).as_ref());
                    s
                }
            }
        );
        println!(
            "- cache: {}",
            match config.cache {
                None => "off".red().bold().to_string(),
                Some(ref opts) => {
                    let mut s = format!("{}", "on".green().bold());
                    s.push(' ');
                    s.push_str(format!("max_age={}", opts.max_age.to_string().bold()).as_ref());
                    s
                }
            }
        );
        println!(
            "- cors: {}",
            match config.cors {
                None => "off".red().bold().to_string(),
                Some(ref opts) => {
                    let mut s = format!("{}", "on".green().bold());
                    s.push(' ');
                    let origin = match opts.origin {
                        None => "*".to_string(),
                        Some(ref urls) => format!("{:?}", urls),
                    };
                    s.push_str(format!("origin={}", origin.bold()).as_ref());
                    s
                }
            }
        );
        // FIXME: Really need to merge config and options. Need the parsed options here, not config.
        println!(
            "- log: {}",
            match config.log {
                None => "off".red().bold().to_string(),
                Some(_) => {
//                    let opts = options::Log;
                    let mut s = format!("{}", "on".green().bold());
                    s.push(' ');
                    s.push_str(&format!("dst={}", "stdout".bold()));
                    s
                }
            }
        );
    } else {
        println!(
            "[hunk] serving \"{}\" at http://{}",
            ctx.root.to_string_lossy(),
            server.local_addr().unwrap()
        );
    }

    server.run().unwrap();
}
