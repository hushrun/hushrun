//! The messages of Xcode's compilation-cache service (B-85), in protobuf's wire format, by hand:
//! `compilation_cache_service.cas.v1` (`CASDBService`: `Get`, `Put`, `Load`, `Save`) and
//! `compilation_cache_service.keyvalue.v1` (`KeyValueDB`: `GetValue`, `PutValue`), as LLVM's
//! `llvm/lib/RemoteCachingService/RemoteCacheProto/*.proto` define them (Apple's fork of LLVM,
//! which Xcode's toolchain plugin speaks).
//!
//! Only what those messages use is here: varints, length-delimited fields, `oneof`, a
//! `map<string, bytes>` and enums. A field this code doesn't know is skipped, as protobuf says
//! a reader must; a message that can't be read is an error, which the service answers as an
//! error and Xcode takes as a miss.

use std::collections::BTreeMap;

/// A request or a reply that isn't the message it should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a malformed message: {}", self.0)
    }
}

impl std::error::Error for Malformed {}

/// The gRPC method paths the service answers.
pub mod path {
    pub const CAS_GET: &str = "/compilation_cache_service.cas.v1.CASDBService/Get";
    pub const CAS_PUT: &str = "/compilation_cache_service.cas.v1.CASDBService/Put";
    pub const CAS_LOAD: &str = "/compilation_cache_service.cas.v1.CASDBService/Load";
    pub const CAS_SAVE: &str = "/compilation_cache_service.cas.v1.CASDBService/Save";
    pub const KV_GET: &str = "/compilation_cache_service.keyvalue.v1.KeyValueDB/GetValue";
    pub const KV_PUT: &str = "/compilation_cache_service.keyvalue.v1.KeyValueDB/PutValue";
}

/// `CASBytes`: the bytes themselves, or a file the other side wrote them to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bytes {
    Data(Vec<u8>),
    FilePath(String),
}

impl Default for Bytes {
    fn default() -> Self {
        Bytes::Data(Vec::new())
    }
}

/// `CASObject`, and `CASBlob` (the same with no references: the two are wire compatible).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Object {
    pub blob: Bytes,
    pub references: Vec<Vec<u8>>,
}

/// `Value`: what one key holds, a map from names to bytes. Kept sorted, so that one value has
/// one encoding.
pub type Value = BTreeMap<String, Vec<u8>>;

/// One request the service reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `CASGetRequest`: an object and its references.
    CasGet { id: Vec<u8>, write_to_disk: bool },
    /// `CASLoadRequest`: an object's bytes alone.
    CasLoad { id: Vec<u8>, write_to_disk: bool },
    /// `CASPutRequest`: an object with its references.
    CasPut(Object),
    /// `CASSaveRequest`: bytes with no references.
    CasSave(Bytes),
    /// `GetValueRequest`.
    KvGet { key: Vec<u8> },
    /// `PutValueRequest`.
    KvPut { key: Vec<u8>, value: Value },
}

impl Request {
    /// Reads the request of the method at `path`. `None`: a method the service doesn't have.
    pub fn read(path: &str, bytes: &[u8]) -> Option<Result<Request, Malformed>> {
        Some(match path {
            path::CAS_GET => read_id_request(bytes)
                .map(|(id, write_to_disk)| Request::CasGet { id, write_to_disk }),
            path::CAS_LOAD => read_id_request(bytes)
                .map(|(id, write_to_disk)| Request::CasLoad { id, write_to_disk }),
            path::CAS_PUT => read_put(bytes).map(Request::CasPut),
            path::CAS_SAVE => read_save(bytes).map(Request::CasSave),
            path::KV_GET => read_kv_get(bytes).map(|key| Request::KvGet { key }),
            path::KV_PUT => read_kv_put(bytes).map(|(key, value)| Request::KvPut { key, value }),
            _ => return None,
        })
    }
}

/// `Outcome` of a get or a load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Error = 0,
    NotFound = 1,
    Success = 2,
}

/// One reply the service writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// `CASGetResponse`.
    CasGot(Result<Option<Object>, String>),
    /// `CASLoadResponse`.
    CasLoaded(Result<Option<Bytes>, String>),
    /// `CASPutResponse` and `CASSaveResponse`: the object's id.
    CasStored(Result<Vec<u8>, String>),
    /// `GetValueResponse`.
    KvGot(Result<Option<Value>, String>),
    /// `PutValueResponse`.
    KvPut(Result<(), String>),
}

impl Reply {
    /// The reply's bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Reply::CasGot(got) => match got {
                Ok(Some(object)) => {
                    put_varint_field(&mut out, 1, Outcome::Success as u64);
                    put_message(&mut out, 3, &encode_object(object));
                }
                Ok(None) => put_varint_field(&mut out, 1, Outcome::NotFound as u64),
                Err(why) => {
                    put_varint_field(&mut out, 1, Outcome::Error as u64);
                    put_message(&mut out, 2, &encode_error(why));
                }
            },
            Reply::CasLoaded(got) => match got {
                Ok(Some(blob)) => {
                    put_varint_field(&mut out, 1, Outcome::Success as u64);
                    let object = Object {
                        blob: blob.clone(),
                        references: Vec::new(),
                    };
                    put_message(&mut out, 3, &encode_object(&object));
                }
                Ok(None) => put_varint_field(&mut out, 1, Outcome::NotFound as u64),
                Err(why) => {
                    put_varint_field(&mut out, 1, Outcome::Error as u64);
                    put_message(&mut out, 2, &encode_error(why));
                }
            },
            Reply::CasStored(stored) => match stored {
                Ok(id) => put_message(&mut out, 2, &encode_id(id)),
                Err(why) => put_message(&mut out, 1, &encode_error(why)),
            },
            Reply::KvGot(got) => match got {
                Ok(Some(value)) => {
                    put_varint_field(&mut out, 1, Outcome::Success as u64);
                    put_message(&mut out, 3, &encode_value(value));
                }
                Ok(None) => put_varint_field(&mut out, 1, Outcome::NotFound as u64),
                Err(why) => {
                    put_varint_field(&mut out, 1, Outcome::Error as u64);
                    put_message(&mut out, 2, &encode_error(why));
                }
            },
            Reply::KvPut(put) => {
                if let Err(why) = put {
                    put_message(&mut out, 1, &encode_error(why));
                }
            }
        }
        out
    }
}

/// A `CASObject`'s bytes: its blob, then its references in order.
pub fn encode_object(object: &Object) -> Vec<u8> {
    let mut out = Vec::new();
    let mut blob = Vec::new();
    match &object.blob {
        Bytes::Data(data) => put_bytes_field(&mut blob, 1, data),
        Bytes::FilePath(path) => put_bytes_field(&mut blob, 2, path.as_bytes()),
    }
    put_message(&mut out, 1, &blob);
    for reference in &object.references {
        put_message(&mut out, 2, &encode_id(reference));
    }
    out
}

/// Reads a `CASObject` (or a `CASBlob`, which is one with no references).
pub fn read_object(bytes: &[u8]) -> Result<Object, Malformed> {
    let mut object = Object::default();
    for field in Fields::new(bytes) {
        match field? {
            (1, Wire::Len(blob)) => object.blob = read_cas_bytes(blob)?,
            (2, Wire::Len(id)) => object.references.push(read_id(id)?),
            _ => {}
        }
    }
    Ok(object)
}

/// A `Value`'s bytes, its entries in the order of their names.
pub fn encode_value(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, bytes) in value {
        let mut entry = Vec::new();
        put_bytes_field(&mut entry, 1, name.as_bytes());
        put_bytes_field(&mut entry, 2, bytes);
        put_message(&mut out, 1, &entry);
    }
    out
}

/// Reads a `Value`. A name that appears twice keeps its last bytes, as protobuf's maps do.
pub fn read_value(bytes: &[u8]) -> Result<Value, Malformed> {
    let mut value = Value::new();
    for field in Fields::new(bytes) {
        if let (1, Wire::Len(entry)) = field? {
            let mut name = String::new();
            let mut data = Vec::new();
            for field in Fields::new(entry) {
                match field? {
                    (1, Wire::Len(text)) => {
                        name = String::from_utf8(text.to_vec())
                            .map_err(|_| Malformed("a value's name is not UTF-8"))?;
                    }
                    (2, Wire::Len(bytes)) => data = bytes.to_vec(),
                    _ => {}
                }
            }
            value.insert(name, data);
        }
    }
    Ok(value)
}

fn encode_id(id: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes_field(&mut out, 1, id);
    out
}

fn encode_error(description: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes_field(&mut out, 1, description.as_bytes());
    out
}

fn read_id(bytes: &[u8]) -> Result<Vec<u8>, Malformed> {
    let mut id = Vec::new();
    for field in Fields::new(bytes) {
        if let (1, Wire::Len(data)) = field? {
            id = data.to_vec();
        }
    }
    Ok(id)
}

fn read_cas_bytes(bytes: &[u8]) -> Result<Bytes, Malformed> {
    let mut out = Bytes::default();
    for field in Fields::new(bytes) {
        match field? {
            (1, Wire::Len(data)) => out = Bytes::Data(data.to_vec()),
            (2, Wire::Len(text)) => {
                out = Bytes::FilePath(
                    String::from_utf8(text.to_vec())
                        .map_err(|_| Malformed("a file path is not UTF-8"))?,
                );
            }
            _ => {}
        }
    }
    Ok(out)
}

fn read_id_request(bytes: &[u8]) -> Result<(Vec<u8>, bool), Malformed> {
    let mut id = Vec::new();
    let mut write_to_disk = false;
    for field in Fields::new(bytes) {
        match field? {
            (1, Wire::Len(data)) => id = read_id(data)?,
            (2, Wire::Varint(flag)) => write_to_disk = flag != 0,
            _ => {}
        }
    }
    Ok((id, write_to_disk))
}

fn read_put(bytes: &[u8]) -> Result<Object, Malformed> {
    let mut object = Object::default();
    for field in Fields::new(bytes) {
        if let (1, Wire::Len(data)) = field? {
            object = read_object(data)?;
        }
    }
    Ok(object)
}

fn read_save(bytes: &[u8]) -> Result<Bytes, Malformed> {
    let mut blob = Bytes::default();
    for field in Fields::new(bytes) {
        if let (1, Wire::Len(data)) = field? {
            blob = read_object(data)?.blob;
        }
    }
    Ok(blob)
}

fn read_kv_get(bytes: &[u8]) -> Result<Vec<u8>, Malformed> {
    let mut key = Vec::new();
    for field in Fields::new(bytes) {
        if let (1, Wire::Len(data)) = field? {
            key = data.to_vec();
        }
    }
    Ok(key)
}

fn read_kv_put(bytes: &[u8]) -> Result<(Vec<u8>, Value), Malformed> {
    let mut key = Vec::new();
    let mut value = Value::new();
    for field in Fields::new(bytes) {
        match field? {
            (1, Wire::Len(data)) => key = data.to_vec(),
            (2, Wire::Len(data)) => value = read_value(data)?,
            _ => {}
        }
    }
    Ok((key, value))
}

/// Reads the replies the service writes: what a client of it (the tests, `hushrun build xcode
/// check`) reads back.
pub mod client {
    use super::*;

    /// A `CASGetResponse` or `CASLoadResponse`: the outcome, and the object when there is one.
    pub fn read_got(bytes: &[u8]) -> Result<(Outcome, Option<Object>, Option<String>), Malformed> {
        let mut outcome = Outcome::Error;
        let mut object = None;
        let mut error = None;
        for field in Fields::new(bytes) {
            match field? {
                (1, Wire::Varint(n)) => outcome = outcome_of(n),
                (2, Wire::Len(data)) => error = Some(read_error(data)?),
                (3, Wire::Len(data)) => object = Some(read_object(data)?),
                _ => {}
            }
        }
        Ok((outcome, object, error))
    }

    /// A `CASPutResponse` or `CASSaveResponse`: the id, or the error.
    pub fn read_stored(bytes: &[u8]) -> Result<Result<Vec<u8>, String>, Malformed> {
        let mut out = Err(String::new());
        for field in Fields::new(bytes) {
            match field? {
                (1, Wire::Len(data)) => out = Err(read_error(data)?),
                (2, Wire::Len(data)) => out = Ok(read_id(data)?),
                _ => {}
            }
        }
        Ok(out)
    }

    /// A `GetValueResponse`.
    pub fn read_kv_got(bytes: &[u8]) -> Result<(Outcome, Option<Value>), Malformed> {
        let mut outcome = Outcome::Error;
        let mut value = None;
        for field in Fields::new(bytes) {
            match field? {
                (1, Wire::Varint(n)) => outcome = outcome_of(n),
                (3, Wire::Len(data)) => value = Some(read_value(data)?),
                _ => {}
            }
        }
        Ok((outcome, value))
    }

    /// A `PutValueResponse`: the error, when it holds one.
    pub fn read_kv_put(bytes: &[u8]) -> Result<Option<String>, Malformed> {
        let mut error = None;
        for field in Fields::new(bytes) {
            if let (1, Wire::Len(data)) = field? {
                error = Some(read_error(data)?);
            }
        }
        Ok(error)
    }

    /// The request bytes of each method, as Xcode's plugin writes them.
    pub fn cas_get(id: &[u8], write_to_disk: bool) -> Vec<u8> {
        let mut out = Vec::new();
        put_message(&mut out, 1, &encode_id(id));
        if write_to_disk {
            put_varint_field(&mut out, 2, 1);
        }
        out
    }

    pub fn cas_put(object: &Object) -> Vec<u8> {
        let mut out = Vec::new();
        put_message(&mut out, 1, &encode_object(object));
        out
    }

    pub fn cas_save(blob: &Bytes) -> Vec<u8> {
        cas_put(&Object {
            blob: blob.clone(),
            references: Vec::new(),
        })
    }

    pub fn kv_get(key: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_bytes_field(&mut out, 1, key);
        out
    }

    pub fn kv_put(key: &[u8], value: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        put_bytes_field(&mut out, 1, key);
        put_message(&mut out, 2, &encode_value(value));
        out
    }

    fn read_error(bytes: &[u8]) -> Result<String, Malformed> {
        let mut text = String::new();
        for field in Fields::new(bytes) {
            if let (1, Wire::Len(data)) = field? {
                text = String::from_utf8_lossy(data).into_owned();
            }
        }
        Ok(text)
    }

    fn outcome_of(n: u64) -> Outcome {
        match n {
            1 => Outcome::NotFound,
            2 => Outcome::Success,
            _ => Outcome::Error,
        }
    }
}

// The wire format.

#[derive(Debug, Clone, Copy)]
enum Wire<'a> {
    Varint(u64),
    Len(&'a [u8]),
    Fixed,
}

/// The fields of one message, in order: `(number, value)`.
struct Fields<'a> {
    bytes: &'a [u8],
    failed: bool,
}

impl<'a> Fields<'a> {
    fn new(bytes: &'a [u8]) -> Fields<'a> {
        Fields {
            bytes,
            failed: false,
        }
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Wire<'a>), Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.bytes.is_empty() || self.failed {
            return None;
        }
        let result = self.read();
        if result.is_err() {
            self.failed = true;
        }
        Some(result)
    }
}

impl<'a> Fields<'a> {
    fn read(&mut self) -> Result<(u64, Wire<'a>), Malformed> {
        let tag = varint(&mut self.bytes)?;
        let number = tag >> 3;
        if number == 0 {
            return Err(Malformed("a field numbered 0"));
        }
        let wire = match tag & 7 {
            0 => Wire::Varint(varint(&mut self.bytes)?),
            1 => {
                self.skip(8)?;
                Wire::Fixed
            }
            2 => {
                let len = usize::try_from(varint(&mut self.bytes)?)
                    .map_err(|_| Malformed("a length past memory"))?;
                if len > self.bytes.len() {
                    return Err(Malformed("a field longer than its message"));
                }
                let (data, rest) = self.bytes.split_at(len);
                self.bytes = rest;
                Wire::Len(data)
            }
            5 => {
                self.skip(4)?;
                Wire::Fixed
            }
            _ => return Err(Malformed("a wire type protobuf 3 doesn't write")),
        };
        Ok((number, wire))
    }

    fn skip(&mut self, n: usize) -> Result<(), Malformed> {
        if self.bytes.len() < n {
            return Err(Malformed("a fixed field past its message"));
        }
        self.bytes = &self.bytes[n..];
        Ok(())
    }
}

fn varint(bytes: &mut &[u8]) -> Result<u64, Malformed> {
    let mut value = 0u64;
    for i in 0..10 {
        let Some((&byte, rest)) = bytes.split_first() else {
            return Err(Malformed("a varint past its message"));
        };
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Malformed("a varint longer than 10 bytes"))
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_varint_field(out: &mut Vec<u8>, number: u64, value: u64) {
    put_varint(out, number << 3);
    put_varint(out, value);
}

fn put_bytes_field(out: &mut Vec<u8>, number: u64, bytes: &[u8]) {
    put_varint(out, (number << 3) | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_message(out: &mut Vec<u8>, number: u64, message: &[u8]) {
    put_bytes_field(out, number, message);
}

#[cfg(test)]
mod tests {
    use super::client;
    use super::*;

    // covers: B-85
    #[test]
    fn an_object_reads_back_as_written() {
        let object = Object {
            blob: Bytes::Data(b"object bytes".to_vec()),
            references: vec![vec![1; 32], vec![2; 32]],
        };
        assert_eq!(read_object(&encode_object(&object)).unwrap(), object);
        let path = Object {
            blob: Bytes::FilePath("/tmp/x.blob".into()),
            references: vec![],
        };
        assert_eq!(read_object(&encode_object(&path)).unwrap(), path);
    }

    // covers: B-85
    #[test]
    fn every_request_reads_as_its_method() {
        let id = vec![7u8; 32];
        assert_eq!(
            Request::read(path::CAS_GET, &client::cas_get(&id, true)),
            Some(Ok(Request::CasGet {
                id: id.clone(),
                write_to_disk: true
            }))
        );
        assert_eq!(
            Request::read(path::CAS_LOAD, &client::cas_get(&id, false)),
            Some(Ok(Request::CasLoad {
                id: id.clone(),
                write_to_disk: false
            }))
        );
        let object = Object {
            blob: Bytes::Data(b"o".to_vec()),
            references: vec![id.clone()],
        };
        assert_eq!(
            Request::read(path::CAS_PUT, &client::cas_put(&object)),
            Some(Ok(Request::CasPut(object)))
        );
        assert_eq!(
            Request::read(
                path::CAS_SAVE,
                &client::cas_save(&Bytes::Data(b"b".to_vec()))
            ),
            Some(Ok(Request::CasSave(Bytes::Data(b"b".to_vec()))))
        );
        let value: Value = [("value".to_string(), b"v".to_vec())].into();
        assert_eq!(
            Request::read(path::KV_PUT, &client::kv_put(b"key", &value)),
            Some(Ok(Request::KvPut {
                key: b"key".to_vec(),
                value: value.clone()
            }))
        );
        assert_eq!(
            Request::read(path::KV_GET, &client::kv_get(b"key")),
            Some(Ok(Request::KvGet {
                key: b"key".to_vec()
            }))
        );
        assert_eq!(Request::read("/other.Service/Method", b""), None);
    }

    // covers: B-85
    #[test]
    fn a_value_has_one_encoding_whatever_the_order_it_came_in() {
        // Two entries written in the order a's map might give them, then read: the same value,
        // and the same bytes once written again.
        let mut one = Vec::new();
        for (name, data) in [("b", b"2".as_slice()), ("a", b"1".as_slice())] {
            let mut entry = Vec::new();
            put_bytes_field(&mut entry, 1, name.as_bytes());
            put_bytes_field(&mut entry, 2, data);
            put_message(&mut one, 1, &entry);
        }
        let value = read_value(&one).unwrap();
        let mut two = Vec::new();
        for (name, data) in [("a", b"1".as_slice()), ("b", b"2".as_slice())] {
            let mut entry = Vec::new();
            put_bytes_field(&mut entry, 1, name.as_bytes());
            put_bytes_field(&mut entry, 2, data);
            put_message(&mut two, 1, &entry);
        }
        assert_eq!(value, read_value(&two).unwrap());
        assert_eq!(encode_value(&value), two);
    }

    // covers: B-85
    #[test]
    fn unknown_fields_are_skipped_and_broken_ones_refused() {
        let mut bytes = client::kv_get(b"key");
        // A varint field, a fixed64 and a fixed32 this code doesn't know.
        put_varint_field(&mut bytes, 9, 300);
        put_varint(&mut bytes, (10 << 3) | 1);
        bytes.extend_from_slice(&[0; 8]);
        put_varint(&mut bytes, (11 << 3) | 5);
        bytes.extend_from_slice(&[0; 4]);
        assert_eq!(
            Request::read(path::KV_GET, &bytes),
            Some(Ok(Request::KvGet {
                key: b"key".to_vec()
            }))
        );
        // A length past the message's end.
        assert!(matches!(
            Request::read(path::KV_GET, &[0x0a, 0x10, 1, 2]),
            Some(Err(_))
        ));
        // A group (wire type 3), which protobuf 3 never writes.
        assert!(matches!(Request::read(path::KV_GET, &[0x0b]), Some(Err(_))));
    }

    // covers: B-85
    #[test]
    fn replies_read_back_by_a_client() {
        let object = Object {
            blob: Bytes::Data(b"o".to_vec()),
            references: vec![vec![3; 32]],
        };
        let got = Reply::CasGot(Ok(Some(object.clone()))).encode();
        assert_eq!(
            client::read_got(&got).unwrap(),
            (Outcome::Success, Some(object), None)
        );
        let missing = Reply::CasGot(Ok(None)).encode();
        assert_eq!(
            client::read_got(&missing).unwrap(),
            (Outcome::NotFound, None, None)
        );
        let failed = Reply::CasLoaded(Err("no".into())).encode();
        assert_eq!(
            client::read_got(&failed).unwrap(),
            (Outcome::Error, None, Some("no".into()))
        );
        let stored = Reply::CasStored(Ok(vec![9; 32])).encode();
        assert_eq!(client::read_stored(&stored).unwrap(), Ok(vec![9; 32]));
        let value: Value = [("value".to_string(), b"v".to_vec())].into();
        let kv = Reply::KvGot(Ok(Some(value.clone()))).encode();
        assert_eq!(
            client::read_kv_got(&kv).unwrap(),
            (Outcome::Success, Some(value))
        );
        assert_eq!(
            client::read_kv_put(&Reply::KvPut(Ok(())).encode()),
            Ok(None)
        );
        assert_eq!(
            client::read_kv_put(&Reply::KvPut(Err("full".into())).encode()),
            Ok(Some("full".into()))
        );
    }
}
