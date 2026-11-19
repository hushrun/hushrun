//! clang's record of the include graph (B-55): which file included which, written to a file of
//! the launcher's when the compile's environment says `CC_PRINT_HEADERS_FORMAT=json`,
//! `CC_PRINT_HEADERS_FILTERING=direct-per-file` and `CC_PRINT_HEADERS_FILE=<path>`.
//!
//! Each record names a file and the files it included directly. clang leaves out what system
//! headers include; for those, and for a compiler that writes no graph, the source's folder
//! stands for the including file's (B-54).

use std::collections::BTreeMap;

use serde::Deserialize;

/// The variables that make clang write the graph.
pub const FORMAT: (&str, &str) = ("CC_PRINT_HEADERS_FORMAT", "json");
/// See [`FORMAT`].
pub const FILTERING: (&str, &str) = ("CC_PRINT_HEADERS_FILTERING", "direct-per-file");
/// See [`FORMAT`]: its value is the file.
pub const FILE: &str = "CC_PRINT_HEADERS_FILE";

#[derive(Deserialize)]
struct Graph {
    #[serde(default)]
    dependencies: Vec<Record>,
}

#[derive(Deserialize)]
struct Record {
    source: String,
    #[serde(default)]
    includes: Vec<Include>,
}

#[derive(Deserialize)]
struct Include {
    file: String,
}

/// For each included file as clang named it, the files that included it, in order.
pub type Includers = BTreeMap<String, Vec<String>>;

/// Reads the graph clang wrote. Several JSON documents may follow one another (clang appends);
/// text that isn't the graph gives `None`, and the source's folder stands in.
pub fn read(text: &str) -> Option<Includers> {
    let mut includers = Includers::new();
    let mut any = false;
    for doc in serde_json::Deserializer::from_str(text).into_iter::<Graph>() {
        let graph = doc.ok()?;
        any = true;
        for record in graph.dependencies {
            for include in record.includes {
                let by = includers.entry(include.file).or_default();
                if !by.contains(&record.source) {
                    by.push(record.source.clone());
                }
            }
        }
    }
    any.then_some(includers)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-55
    #[test]
    fn apple_clang_s_graph_names_each_file_s_includer() {
        let text = r#"{"version":"2.0.0","dependencies":[{"source":"m.cc","includes":[{"location":"m.cc:1:1","file":"inc/a.h"},{"location":"m.cc:2:1","file":"/sdk/usr/include/c++/v1/stdio.h"}],"imports":[]},{"source":"inc/a.h","includes":[{"location":"inc/a.h:1:1","file":"inc/b.h"}],"imports":[]}]}"#;
        let graph = read(text).unwrap();
        assert_eq!(graph["inc/a.h"], ["m.cc"]);
        assert_eq!(graph["inc/b.h"], ["inc/a.h"]);
        assert!(!graph.contains_key("m.cc"));
        assert!(read("not json").is_none());
        assert!(read("").is_none());
    }
}
