import XCTest
import Testing
import Foundation
@testable import Lib

final class LibTests: XCTestCase {
    func testLib() { XCTAssertEqual(lib(), 42) }
    func testFlips() { XCTAssertNil(ProcessInfo.processInfo.environment["FAIL"]) }
}

@Suite struct LibSuite {
    @Test func swiftTesting() { #expect(lib() == 42) }
}
