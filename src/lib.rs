#![feature(ip_constructors)]
#![feature(option_filter)]
#![feature(proc_macro)] // For maud

extern crate flate2;
extern crate futures;
extern crate futures_cpupool;
extern crate hyper;
#[macro_use]
extern crate lazy_static;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate unicase;
extern crate chrono;
extern crate maud;
extern crate pretty_bytes;

// 3rd party

use futures::{stream, Future};
use futures_cpupool::CpuPool;

use hyper::{Method, StatusCode};
use hyper::server::{Request, Response};
use hyper::header;

use unicase::Ascii;


// Std

use std::fs::File;
use std::path::{self, Path, PathBuf};

// 1st party

mod range;
mod negotiation;
mod mime;
mod base36;
mod util;
mod chunks;
mod resource;
mod gzip;
mod cors;
mod browse;
pub mod logger;
pub mod config;
pub mod options;

use range::RequestedRange;
use chunks::ChunkStream;
use resource::Resource;

const CHUNK_SIZE: u64 = 65_536;

pub struct Context {
    pub root: PathBuf,
    pub pool: CpuPool,
    pub opts: options::Options,
}

pub struct HttpService(&'static Context);

impl HttpService {
    pub fn new(ctx: &'static Context) -> HttpService {
        HttpService(ctx)
    }
}

fn is_not_modified(resource: &Resource, req: &Request, resource_etag: &header::EntityTag) -> bool {
    if !negotiation::none_match(req.headers().get::<header::IfNoneMatch>(), resource_etag) {
        true
    } else if let Some(&header::IfModifiedSince(since)) = req.headers().get() {
        resource.last_modified() <= since
    } else {
        false
    }
}

fn is_precondition_failed(
    resource: &Resource,
    req: &Request,
    resource_etag: &header::EntityTag,
) -> bool {
    if !negotiation::any_match(req.headers().get::<header::IfMatch>(), resource_etag) {
        true
    } else if let Some(&header::IfUnmodifiedSince(since)) = req.headers().get() {
        resource.last_modified() > since
    } else {
        false
    }
}


fn handler(ctx: &'static Context, req: &Request) -> Response<ChunkStream> {
    if *req.method() != Method::Get && *req.method() != Method::Head
        && *req.method() != Method::Options
    {
        return method_not_allowed();
    }

    let resource_path = match get_resource_path(&ctx.root, req.uri().path()) {
        None => return not_found(),
        Some(path) => path,
    };

    let file = match File::open(&resource_path) {
        Err(_) => return not_found(),
        Ok(file) => file,
    };

    if file.metadata().unwrap().is_dir() {
        if ctx.opts.browse {
            return browse::handle_folder(ctx.root.as_path(), resource_path.as_path());
        } else {
            return not_found()
        }
    }

    let resource = match Resource::new(
        file,
        ctx.pool.clone(),
        mime::guess_mime_by_path(resource_path.as_path()),
    ) {
        Err(_) => return not_found(),
        Ok(resource) => resource,
    };

    // CORS
    // https://www.w3.org/TR/cors/#resource-processing-model
    // NOTE: The string "*" cannot be used for a resource that supports credentials.

    let mut res: Response<ChunkStream> = Response::new();

    if cors::handle_cors(ctx.opts.cors.as_ref(), req, &mut res) {
        return res;
    }

    // HANDLE CACHING HEADERS

    let should_gzip = ctx.opts
        .gzip
        .as_ref()
        .map(|opts| {
            let compressible = resource.content_type().compressible || resource_path.extension()
                .and_then(|x| std::ffi::OsStr::to_str(x))
                .map(|x| Ascii::new(String::from(x)))
                .map(|ext| opts.also_extensions.contains(&ext))
                .unwrap_or(false);
            resource.len() >= opts.threshold && compressible
                && negotiation::negotiate_encoding(req.headers().get::<header::AcceptEncoding>())
                    == Some(header::Encoding::Gzip)
        })
        .unwrap_or(false);

    let resource_etag = resource.etag(!should_gzip);

    if is_not_modified(&resource, req, &resource_etag) {
        return not_modified(resource_etag);
    }

    if is_precondition_failed(&resource, req, &resource_etag) {
        return precondition_failed();
    }

    // PARSE RANGE HEADER
    // - Comes after evaluating precondition headers.
    //   <https://tools.ietf.org/html/rfc7233#section-3.1>

    let range = if should_gzip {
        // Ignore Range if response is gzipped
        RequestedRange::None
    } else {
        range::parse_range_header(
            req.headers().has::<header::Range>(),
            req.headers().get::<header::Range>(),
            resource.len(),
        )
    };

    if let RequestedRange::NotSatisfiable = range {
        return invalid_range(resource.len());
    };

    res.headers_mut().set(header::ETag(resource_etag));
    res.headers_mut()
        .set(header::AcceptRanges(vec![header::RangeUnit::Bytes]));
    res.headers_mut()
        .set(header::LastModified(resource.last_modified()));
    res.headers_mut()
        .set(header::ContentType(resource.content_type().mime.clone()));

    // More about Content-Length: <https://tools.ietf.org/html/rfc2616#section-4.4>
    // - Represents length *after* transfer-encoding.
    // - Don't set Content-Length if Transfer-Encoding != 'identity'
    if should_gzip {
        res.headers_mut()
            .set(header::TransferEncoding(vec![header::Encoding::Chunked]));
    } else {
        res.headers_mut().set(header::ContentLength(resource.len()));
    }

    // Accept-Encoding doesn't affect the response unless gzip is turned on
    if ctx.opts.gzip.is_some() {
        res.headers_mut().set(header::Vary::Items(vec![
            unicase::Ascii::new("Accept-Encoding".to_owned()),
        ]));
    }

    // Only set max-age if it's configured at all.
    if let Some(max_age) = ctx.opts.cache.as_ref().map(|opts| opts.max_age) {
        res.headers_mut().set(header::CacheControl(vec![
            header::CacheDirective::Public,
            header::CacheDirective::MaxAge(max_age),
        ]));
    }

    let body: ChunkStream = {
        let range = match range {
            RequestedRange::Satisfiable(mut range) => {
                res.set_status(StatusCode::PartialContent);
                res.headers_mut()
                    .set(header::ContentRange(header::ContentRangeSpec::Bytes {
                        range: Some((range.start, range.end)),
                        instance_length: Some(resource.len()),
                    }));

                // NOTE: Range header is end-inclusive but std::ops::Range is end-exclusive.
                range.end += 1;

                range
            }
            _ => 0..resource.len(),
        };

        resource.get_range(range, CHUNK_SIZE)
    };

    if should_gzip {
        res.headers_mut()
            .set(header::ContentEncoding(vec![header::Encoding::Gzip]));
    }

    // For HEAD requests, we do all the work except sending the body.
    if *req.method() == Method::Head {
        return res;
    }

    if should_gzip {
        res.with_body(gzip::encode(body, ctx.opts.gzip.as_ref().unwrap().level))
    } else {
        res.with_body(body)
    }
}

impl hyper::server::Service for HttpService {
    type Request = Request;
    type Response = Response<ChunkStream>;
    type Error = hyper::Error;
    type Future = Box<Future<Item = Self::Response, Error = Self::Error>>;

    fn call(&self, req: Request) -> Self::Future {
        let ctx = self.0;

        let work = move || {
            let res = handler(ctx, &req);

            if let Some(ref log) = ctx.opts.log {
                log.logger.log(&req, &res);

            }

            Ok(res)
        };

        Box::new(ctx.pool.spawn_fn(work))
    }
}

// A path is safe if it doesn't try to /./ or /../
fn is_safe_path(path: &Path) -> bool {
    path.components().all(|c| match c {
        path::Component::RootDir | path::Component::Normal(_) => true,
        _ => false,
    })
}

// Join root with request path to get the asset path candidate.
fn get_resource_path(root: &Path, req_path: &str) -> Option<PathBuf> {
    // request path must be absolute
    if !req_path.starts_with('/') {
        return None;
    }

    // Security: request path cannot climb directories
    if !is_safe_path(Path::new(req_path)) {
        return None;
    };

    let mut final_path = root.to_path_buf();
    final_path.push(&req_path[1..]);

    Some(final_path)
}

// CANNED RESPONSES

fn not_found() -> Response<ChunkStream> {
    let text = b"Not Found";
    let body: ChunkStream = Box::new(stream::once(Ok(text[..].into())));
    Response::new()
        .with_status(StatusCode::NotFound)
        .with_header(header::ContentLength(text.len() as u64))
        .with_body(body)
}

fn precondition_failed() -> Response<ChunkStream> {
    Response::new()
        .with_status(StatusCode::PreconditionFailed)
        .with_header(header::ContentLength(0))
}

fn not_modified(etag: header::EntityTag) -> Response<ChunkStream> {
    Response::new()
        .with_status(StatusCode::NotModified)
        .with_header(header::ETag(etag)) // Required in 304 response
        .with_header(header::ContentLength(0))
}

// TODO: Is OPTIONS part of MethodNotAllowed?
fn method_not_allowed() -> Response<ChunkStream> {
    let text = b"This resource only supports GET, HEAD, and OPTIONS.";
    let body: ChunkStream = Box::new(stream::once(Ok(text[..].into())));
    Response::new()
        .with_status(StatusCode::MethodNotAllowed)
        .with_header(header::ContentLength(text.len() as u64))
        .with_header(header::ContentType::plaintext())
        .with_header(header::Allow(vec![
            Method::Get,
            Method::Head,
            Method::Options,
        ]))
        .with_body(body)
}

fn invalid_range(resource_len: u64) -> Response<ChunkStream> {
    let text = b"Invalid range";
    let body: ChunkStream = Box::new(stream::once(Ok(text[..].into())));
    Response::new()
        .with_status(StatusCode::RangeNotSatisfiable)
        .with_header(header::ContentRange(header::ContentRangeSpec::Bytes {
            range: None,
            instance_length: Some(resource_len),
        }))
        .with_header(header::ContentType::plaintext())
        .with_header(header::ContentLength(text.len() as u64))
        .with_body(body)
}
