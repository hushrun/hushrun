//! \[ADR 0089\] The fixtures of the `swift test` adapter's tests, which `units_swift.rs`
//! (in-process) and `e2e_swift.rs` (the CLI) share. The packages are tiny, and need Swift 6:
//! `HUSHRUN_TEST_SWIFT` names the `swift` to use (by default the one on `PATH`). The tests say so
//! and pass where it is missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The `swift` these tests run, and whether it is Swift 6 or later.
pub fn swift() -> Option<PathBuf> {
    let named = std::env::var_os("HUSHRUN_TEST_SWIFT").map(PathBuf::from);
    let swift = named.unwrap_or_else(|| PathBuf::from("swift"));
    let output = hushrun_testkit::tool::command(&swift)
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let major: u32 = text
        .split("Swift version ")
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    (output.status.success() && major >= 6).then_some(swift)
}

pub fn tools(what: &str, here: bool) -> bool {
    if here {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

pub const MANIFEST: &str = r#"// swift-tools-version: 6.0
import PackageDescription
let package = Package(
  name: "Shop",
  targets: [
    .target(name: "Core"),
    .target(name: "Cart", dependencies: ["Core"]),
    .target(name: "Greet"),
    .testTarget(name: "CartTests", dependencies: ["Cart"]),
    .testTarget(name: "CoreTests", dependencies: ["Core"]),
    .testTarget(name: "GreetTests", dependencies: ["Greet"]),
  ]
)
"#;

/// A shop in Swift: Core, Cart on Core, Greet; XCTest for Cart and Greet, Swift Testing for Core.
/// `FAIL` fails one test of each of Cart's and Core's; `CRASH` crashes them.
pub fn shop(tests: &str) -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", ".build/\n"),
        file("Package.swift", MANIFEST),
        file(
            "Sources/Core/Core.swift",
            "public func price(_ x: Int) -> Int { x * 2 }\n",
        ),
        file(
            "Sources/Cart/Cart.swift",
            "import Core\npublic func total(_ xs: [Int]) -> Int { xs.map(price).reduce(0, +) }\n",
        ),
        file(
            "Sources/Greet/Greet.swift",
            "public func hello() -> String { \"hi\" }\n",
        ),
        file(
            "Tests/CartTests/CartTests.swift",
            r#"import XCTest
import Foundation
@testable import Cart

final class CartTests: XCTestCase {
    func testTotal() { XCTAssertEqual(total([1, 2]), 6) }
    func testTax() { XCTAssertEqual(total([5]), 10) }
    func testFlips() {
        if ProcessInfo.processInfo.environment["CRASH"] != nil { fatalError("crash") }
        XCTAssertNil(ProcessInfo.processInfo.environment["FAIL"])
    }
}
"#,
        ),
        file(
            "Tests/CoreTests/CoreTests.swift",
            r#"import Testing
import Foundation
@testable import Core

@Suite struct PriceTests {
    @Test func price() { #expect(Core.price(3) == 6) }
    @Test func tax() { #expect(Core.price(5) == 10) }
    @Test func flips() {
        if ProcessInfo.processInfo.environment["CRASH"] != nil { fatalError("crash") }
        #expect(ProcessInfo.processInfo.environment["FAIL"] == nil)
    }
}
"#,
        ),
        file(
            "Tests/GreetTests/GreetTests.swift",
            "import XCTest\n@testable import Greet\n\nfinal class GreetTests: XCTestCase {\n    func testHello() { XCTAssertEqual(hello(), \"hi\") }\n}\n",
        ),
        file("notes/a.md", "a\n"),
    ]
}

/// `PATH` with `swift`'s directory first, when it is named by its path, then the test's.
pub fn path(swift: &Path) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if swift.is_absolute()
        && let Some(parent) = swift.parent()
    {
        dirs.push(parent.to_path_buf());
    }
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(dirs).expect("PATH")
}
