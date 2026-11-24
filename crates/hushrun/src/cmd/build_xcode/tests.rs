//! The service over a real socket, with an HTTP/2 client that calls it as Xcode's plugin does.

use super::*;
use hushrun_build::xcode::proto::{Bytes as Blob, Object, Outcome, Value, client, path};
use hushrun_build::xcode::service::Config;

struct Client {
    runtime: tokio::runtime::Runtime,
    send: h2::client::SendRequest<Bytes>,
}

impl Client {
    fn connect(socket: &Path) -> Client {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let socket = socket.to_path_buf();
        let send = runtime.block_on(async move {
            let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            let (send, conn) = h2::client::handshake(stream).await.unwrap();
            tokio::spawn(async move {
                let _ = conn.await;
            });
            send
        });
        Client { runtime, send }
    }

    /// Calls `method` with `message`: the grpc-status, and the reply's message when one came.
    fn call(&mut self, method: &str, message: &[u8]) -> (String, Option<Vec<u8>>) {
        let body = frame(message);
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("http://localhost{method}"))
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            .body(())
            .unwrap();
        let send = self.send.clone();
        self.runtime.block_on(async move {
            let mut send = send.ready().await.unwrap();
            let (response, mut stream) = send.send_request(request, false).unwrap();
            stream.send_data(Bytes::from(body), true).unwrap();
            let response = response.await.unwrap();
            let headers_status = response
                .headers()
                .get("grpc-status")
                .map(|v| v.to_str().unwrap().to_string());
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            while let Some(chunk) = body.data().await {
                let chunk = chunk.unwrap();
                let _ = body.flow_control().release_capacity(chunk.len());
                bytes.extend_from_slice(&chunk);
            }
            let trailers = body.trailers().await.unwrap();
            let status = trailers
                .and_then(|t| {
                    t.get("grpc-status")
                        .map(|v| v.to_str().unwrap().to_string())
                })
                .or(headers_status)
                .unwrap_or_default();
            let message = (bytes.len() >= 5).then(|| bytes[5..].to_vec());
            (status, message)
        })
    }
}

fn service(dir: &Path) -> Service {
    Service::new(Config {
        store: dir.join("store"),
        current: None,
        platform: Some("xcode-27c5032a".into()),
        folders: vec![],
        staging: dir.join("staging"),
    })
}

// covers: B-85
#[test]
fn the_plugin_puts_objects_and_a_value_and_gets_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let running = start_at(service(dir.path()), &dir.path().join("s/xcode.sock")).unwrap();
    let mut client = Client::connect(&running.socket);

    let (status, reply) = client.call(
        path::CAS_SAVE,
        &client::cas_save(&Blob::Data(b"obj".to_vec())),
    );
    assert_eq!(status, "0");
    let object = client::read_stored(&reply.unwrap()).unwrap().unwrap();

    let result = Object {
        blob: Blob::Data(b"result".to_vec()),
        references: vec![object.clone()],
    };
    let value: Value = [("value".to_string(), proto::encode_object(&result))].into();
    let (status, reply) = client.call(path::KV_PUT, &client::kv_put(b"key", &value));
    assert_eq!(status, "0");
    assert_eq!(client::read_kv_put(&reply.unwrap()).unwrap(), None);

    let (status, reply) = client.call(path::KV_GET, &client::kv_get(b"key"));
    assert_eq!(status, "0");
    assert_eq!(
        client::read_kv_got(&reply.unwrap()).unwrap(),
        (Outcome::Success, Some(value))
    );
    let (_, reply) = client.call(path::KV_GET, &client::kv_get(b"other"));
    assert_eq!(
        client::read_kv_got(&reply.unwrap()).unwrap(),
        (Outcome::NotFound, None)
    );

    let (status, reply) = client.call(path::CAS_LOAD, &client::cas_get(&object, false));
    assert_eq!(status, "0");
    let (outcome, got, _) = client::read_got(&reply.unwrap()).unwrap();
    assert_eq!(outcome, Outcome::Success);
    assert_eq!(got.unwrap().blob, Blob::Data(b"obj".to_vec()));
    let counts = running.stop(15);
    assert_eq!(counts.asked, 2);
    assert_eq!(counts.local, 1);
    assert_eq!(counts.put, 1);
}

// covers: B-85
#[test]
fn a_large_object_asked_for_on_disk_comes_as_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let running = start_at(service(dir.path()), &dir.path().join("s/xcode.sock")).unwrap();
    let mut client = Client::connect(&running.socket);
    let large = vec![7u8; 3 << 20];
    let (status, reply) = client.call(
        path::CAS_SAVE,
        &client::cas_save(&Blob::Data(large.clone())),
    );
    assert_eq!(status, "0");
    let id = client::read_stored(&reply.unwrap()).unwrap().unwrap();
    let (_, reply) = client.call(path::CAS_LOAD, &client::cas_get(&id, true));
    let (_, got, _) = client::read_got(&reply.unwrap()).unwrap();
    let Blob::FilePath(file) = got.unwrap().blob else {
        panic!("not a file");
    };
    assert_eq!(std::fs::read(&file).unwrap(), large);
    // Not asked for on disk: the bytes themselves.
    let (_, reply) = client.call(path::CAS_LOAD, &client::cas_get(&id, false));
    let (_, got, _) = client::read_got(&reply.unwrap()).unwrap();
    assert_eq!(got.unwrap().blob, Blob::Data(large));
}

// covers: B-85
#[test]
fn what_the_service_cant_answer_is_a_status_the_plugin_reads_as_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let running = start_at(service(dir.path()), &dir.path().join("s/xcode.sock")).unwrap();
    let mut client = Client::connect(&running.socket);
    let (status, reply) = client.call("/other.Service/Method", b"");
    assert_eq!(status, code::UNIMPLEMENTED);
    assert_eq!(reply, None);
    let (status, _) = client.call(path::KV_GET, &[0x0a, 0x10, 1]);
    assert_eq!(status, code::INVALID_ARGUMENT);
}

// covers: B-85
#[test]
fn a_frame_holds_one_whole_message() {
    assert_eq!(unframe(&frame(b"abc")), Ok(&b"abc"[..]));
    assert!(unframe(b"").is_err());
    let mut compressed = frame(b"abc");
    compressed[0] = 1;
    assert_eq!(unframe(&compressed).unwrap_err().0, code::UNIMPLEMENTED);
    let mut short = frame(b"abc");
    short.pop();
    assert!(unframe(&short).is_err());
    assert_eq!(percent("a b%é"), "a b%25%C3%A9");
}
