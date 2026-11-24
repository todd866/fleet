import XCTest

final class StackJourney: XCTestCase {
    func testFindPlacementSourceFromFleetTab() throws {
        let app = XCUIApplication()
        app.launch()
        let fleetTab = app.buttons["Fleet"]
        XCTAssertTrue(fleetTab.waitForExistence(timeout: 5))
        fleetTab.tap()
        app.buttons["Stack"].tap()
        app.buttons["fleet.stack.guide"].tap()
        app.buttons["fleet.stack.layer.Data placement"].tap()
        let source = app.staticTexts["../placement/README.md"]
        XCTAssertTrue(source.waitForExistence(timeout: 5))
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Data placement sources"
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }
}
