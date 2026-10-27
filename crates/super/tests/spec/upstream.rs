//! An HTTP/1.1 upstream keeping each request as it came (its request line, its header lines in
//! order, duplicates included, and its body), answering by path:
//!
//! - `/connection-headers`: with `Connection`, a field it lists, `Keep-Alive` and
//!   `Proxy-Connection`, which aren't to reach the client
//! - `/no-content`: `204` (with a `Content-Length` it can't have), `/not-modified`: `304`
//! - otherwise `200` and `ok` (none for a `HEAD`, whose `Content-Length` is `2`)

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

/// A request as the upstream got it
#[derive(Clone, Debug)]
pub struct Received {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Received {
    /// The values of `name`, one per header line
    pub fn all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    pub fn has(&self, name: &str) -> bool {
        !self.all(name).is_empty()
    }
}

#[derive(Clone)]
pub struct Upstream {
    pub port: u16,
    received: Arc<Mutex<Vec<Received>>>,
}

impl Upstream {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = Arc::new(Mutex::new(vec![]));
        let kept = received.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, kept.clone()));
            }
        });
        Self { port, received }
    }

    /// The requests to `path` (with or without a query) the upstream got
    pub fn requests(&self, path: &str) -> Vec<Received> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.target.split('?').next() == Some(path))
            .cloned()
            .collect()
    }

    /// The request to `path`, waiting a while for it: `None` when none came
    pub async fn request(&self, path: &str) -> Option<Received> {
        for _ in 0..20 {
            if let Some(request) = self.requests(path).pop() {
                return Some(request);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        None
    }
}

async fn serve(stream: TcpStream, received: Arc<Mutex<Vec<Received>>>) {
    let mut reader = BufReader::new(stream);
    while let Some(request) = read(&mut reader).await {
        let answer = answer(&request);
        received.lock().unwrap().push(request);
        if reader.get_mut().write_all(answer.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// A request, its body read as its `Content-Length` or chunks say
async fn read(reader: &mut BufReader<TcpStream>) -> Option<Received> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .ok()
        .filter(|read| *read > 0)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut headers = vec![];
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .ok()
            .filter(|read| *read > 0)?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.to_string(), value.trim().to_string()));
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    let body = match header("transfer-encoding") {
        Some(coding) if coding.contains("chunked") => chunked(reader).await?,
        _ => {
            let length = header("content-length").and_then(|length| length.parse().ok());
            let mut body = vec![0; length.unwrap_or(0)];
            reader.read_exact(&mut body).await.ok()?;
            body
        }
    };
    Some(Received {
        method,
        target,
        headers,
        body,
    })
}

async fn chunked(reader: &mut BufReader<TcpStream>) -> Option<Vec<u8>> {
    let mut body = vec![];
    loop {
        let mut size = String::new();
        reader.read_line(&mut size).await.ok()?;
        let size = usize::from_str_radix(size.trim().split(';').next()?, 16).ok()?;
        let mut chunk = vec![0; size + 2];
        reader.read_exact(&mut chunk).await.ok()?;
        if size == 0 {
            return Some(body);
        }
        body.extend_from_slice(chunk.get(..size)?);
    }
}

fn answer(request: &Received) -> String {
    let path = request.target.split('?').next().unwrap_or_default();
    match path {
        "/connection-headers" => "HTTP/1.1 200 OK\r\nConnection: x-upstream-private\r\n\
             X-Upstream-Private: 1\r\nKeep-Alive: timeout=5\r\nProxy-Connection: keep-alive\r\n\
             Content-Length: 2\r\n\r\nok"
            .to_string(),
        // `Content-Length` on a `204`, which a proxy mustn't pass on
        "/no-content" => "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n".to_string(),
        "/not-modified" => "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\n\r\n".to_string(),
        _ if request.method == "HEAD" => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n".to_string(),
        _ => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_string(),
    }
}
