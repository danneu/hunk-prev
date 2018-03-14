use chunks::ChunkStream;
use pretty_bytes;
use futures::stream;
use hyper;
use hyper::Response;
use hyper::header;
use std::io;
use std::fs::{self, DirEntry};
use std::path::Path;

use maud::{DOCTYPE, html, PreEscaped};

const CSS: &str = r#"
    a {
        text-decoration: none;
        display: inline-block;
        width: 100%;
    }
    a::before {
        display: inline-block;
        vertical-align: middle;
        margin-right: 10px;
    }
    a[class="folder"]::before {
        color: #fdcb6e;
        content: url("data:image/svg+xml; utf8, <svg xmlns='http://www.w3.org/2000/svg' width='16' height='16' viewBox='0 0 64 64'><path fill='rgb(253, 203, 110)' stroke='currentColor' stroke-width='4px' stroke-miterlimit='10' d='M56,53.71H8.17L8,21.06a2.13,2.13,0,0,1,2.13-2.13h2.33l2.13-4.28A4.78,4.78,0,0,1,18.87,12h9.65a4.78,4.78,0,0,1,4.28,2.65l2.13,4.28H52.29a3.55,3.55,0,0,1,3.55,3.55Z'/></svg>");
    }
    a[class="file"]::before {
        content: url("data:image/svg+xml; utf8, <svg xmlns='http://www.w3.org/2000/svg' width='16' height='16' viewBox='0 0 64 64'><g><path fill='transparent' stroke='currentColor' stroke-width='4px' stroke-miterlimit='10' d='M50.46,56H13.54V8H35.85a4.38,4.38,0,0,1,3.1,1.28L49.18,19.52a4.38,4.38,0,0,1,1.28,3.1Z'/><polyline fill='transparent' stroke='currentColor' stroke-width='2px' stroke-miterlimit='10' points='35.29 8.31 35.29 23.03 49.35 23.03'/></g></svg>");
    }
"#;

// TODO: Error handling. This panics on any of the io::Result failures.
pub fn handle_folder(root: &Path, path: &Path) -> Response<ChunkStream> {
    let entries: Vec<DirEntry> = fs::read_dir(path).unwrap().collect::<io::Result<Vec<DirEntry>>>().unwrap();

    // Sort folders first
    let mut entries = entries.into_iter().map(|entry| {
        fs::metadata(entry.path()).map(|metadata| {
            let filename = entry.path().file_name().unwrap().to_string_lossy().to_string();
            let href = format!("/{}", entry.path().strip_prefix(root).unwrap().to_string_lossy().to_string());
            (
                filename,
                href,
                metadata
            )
        })
    })
        .collect::<io::Result<Vec<(String, String, fs::Metadata)>>>()
        .unwrap();

    // Sort folders first, and the sort by filename a-z
    entries.sort_unstable_by_key(|&(ref filename, _, ref metadata)| {
        (!metadata.is_dir(), filename.to_lowercase())
    });

    let parent_href = path.parent()
        .filter(|parent| parent.starts_with(root))
        .and_then(|parent| parent.strip_prefix(root).ok())
        .and_then(|path| path.to_str())
        .map(|path| format!("/{}", path));

    let html = html! {
        (DOCTYPE)
        html lang="en" {
            style (PreEscaped(CSS))
            table style="width: 100%" {
                thead {
                    tr {
                        th ""
                        th ""
                    }
                }

                tbody{
                    @if let Some(href) = parent_href {
                        tr {
                            td a.folder href=(href) ".."
                            td ""
                        }
                    }

                    @for (filename, href, metadata) in entries {
                        tr {
                            td a class=(if metadata.is_dir() { "folder" } else { "file" }) href=(href) {
                                @if metadata.is_dir() {
                                    (format!("{}/", filename))
                                } @else {
                                    (filename)
                                }
                            }
                            td (pretty_bytes::converter::convert(metadata.len() as f64))
                        }
                    }
                }
            }
        }
    };

    let html = html.into_string();
    let len = html.len();
    let body: ChunkStream = Box::new(stream::once(Ok(hyper::Chunk::from(html))));
    Response::new()
        .with_header(header::ContentLength(len as u64))
        .with_header(header::ContentType::html())
        .with_body(body)
}