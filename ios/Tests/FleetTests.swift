import XCTest
@testable import Fleet

final class FleetTests: XCTestCase {
    func testDecodeOverviewPayload() throws {
        let json = """
        {
          "generated_at": 1795474800,
          "date": "2026-11-24",
          "counts": {"pass": 310, "warn": 18, "fail": 3},
          "cadence": {"text": "every 15m", "next": 1795475700, "every": "15m"},
          "agy_morning": "## Morning Summary\\nAll systems nominal.",
          "hosts": [
            {
              "id": "primary",
              "collected_at": 1795474790,
              "ok": true,
              "owner_active": true,
              "chip_c": 61.5,
              "thermal": "nominal",
              "cpu_pct": 14.2,
              "mem_pressure": "normal",
              "mem_used_gb": 18.4,
              "mem_total_gb": 36.0,
              "load_1m": 2.1,
              "batt_pct": 100,
              "ac": true,
              "disk_free_gb": 557.5,
              "disk_total_gb": 994.7,
              "disk_target_gb": 500.0,
              "disk_floor_gb": 150.0,
              "syncthing": {"errors": 0, "out_of_sync": 0, "down": false},
              "glancebar": {"state": "ok", "behind": 0},
              "free_space_guard": {"state": "pass"},
              "unread_mail": 1,
              "failing_jobs_count": 0,
              "failing_jobs": [],
              "fail_count": 0,
              "warn_count": 1,
              "drives": [
                {"name": "archive-a", "mounted": true, "free_gb": 1200.0, "total_gb": 4000.0, "trend_24h_gb": -1.5, "alert": null}
              ],
              "samples": [
                {"t": 1795474190, "chip_c": 58.0, "cpu": 10.0, "disk_free_gb": 557.0},
                {"t": 1795474790, "chip_c": 61.5, "cpu": 14.2, "disk_free_gb": 557.5}
              ]
            }
          ],
          "capacity": {
            "generated_at": "2026-11-23T23:00:00Z",
            "route": {"build": "gemini", "review": "zai"},
            "paused": ["claude", "codex"],
            "pools": [
              {
                "engine": "gemini",
                "pool": "pro",
                "remaining_pct": 94.0,
                "jobs_left": 18,
                "tokens_left": 900000,
                "reset_at": "2026-11-24T06:00:00Z",
                "stale": false,
                "freshness": "fresh",
                "paused": false,
                "unavailable": false
              }
            ]
          },
          "backups": {
            "idrive": {
              "checked_at": 1795474700,
              "host_vm": "vm-backup",
              "container": "idrive-ingest",
              "running": false,
              "appending": false,
              "percent": 100.0,
              "sent_bytes": 1024,
              "total_bytes": 1024,
              "phase": "complete",
              "last_success_epoch": 1795460000,
              "last_success_status": "ok",
              "storage": "4.2 TB"
            },
            "gdrive": {"total": 12, "active_runs": 0, "ok_count": 12},
            "home_mirror": {
              "primary": {"checked_at": 1795474000, "state": "ok", "planned_bytes": 0}
            },
            "placement_violations": 1
          },
          "security": {
            "status": "pass",
            "value": "chain intact",
            "detail": "0 errors",
            "generated": 1795474852,
            "rows": []
          },
          "unread_mail": 2,
          "active_claims": 1
        }
        """.data(using: .utf8)!

        let decoded = try JSONDecoder().decode(FleetOverviewResponse.self, from: json)
        XCTAssertEqual(decoded.counts.pass, 310)
        XCTAssertEqual(decoded.hosts.first?.id, "primary")
        XCTAssertEqual(decoded.hosts.first?.drives.orEmpty.count, 1)
        XCTAssertEqual(decoded.hosts.first?.samples.orEmpty.count, 2)
        XCTAssertEqual(decoded.capacity.paused, ["claude", "codex"])
        XCTAssertEqual(decoded.backups.placementViolations, 1)
        XCTAssertEqual(decoded.security.status, "pass")
    }

    func testDecodeMorningFlexibleRows() throws {
        let json = """
        {
          "version": 1,
          "generated": 1795474800,
          "date": "2026-11-24",
          "ok": true,
          "counts": {"pass": 1, "warn": 1, "fail": 1},
          "rows": [
            {"key": "disk:primary", "status": "pass", "area": "disk", "name": "Root disk", "value": 557.5, "datum": 100, "detail": "healthy"},
            {"key": "job:hub", "status": "fail", "area": "jobs", "name": "Backup daemon", "value": false, "datum": -9, "detail": "killed"},
            {"key": "sec:hub", "status": "warn", "area": "security", "name": "Chain check", "value": "stale", "datum": "12m", "detail": "slow ship"}
          ]
        }
        """.data(using: .utf8)!

        let decoded = try JSONDecoder().decode(MorningResponse.self, from: json)
        XCTAssertEqual(decoded.rows.count, 3)
        XCTAssertEqual(decoded.rows[0].value, "557.5")
        XCTAssertEqual(decoded.rows[0].datum, "100")
        XCTAssertEqual(decoded.rows[1].value, "false")
        XCTAssertEqual(decoded.rows[1].datum, "-9")

        let sortedAll = filterAndSortRows(decoded.rows, filter: .all)
        XCTAssertEqual(sortedAll.map(\.status), ["fail", "warn", "pass"])

        let secOnly = filterAndSortRows(decoded.rows, filter: .security)
        XCTAssertEqual(secOnly.map(\.key), ["sec:hub"])

        let searched = filterAndSortRows(decoded.rows, filter: .all, searchText: "daemon")
        XCTAssertEqual(searched.map(\.key), ["job:hub"])
    }

    func testDecodeSecurityAndMailPayloads() throws {
        let secJSON = """
        {
          "status": "pass",
          "value": "chain pass",
          "generated": 1795474852,
          "local_tripwire_open": 0,
          "namespace_audits": {
            "primary": {
              "status": "pass",
              "fail_count": 0,
              "warn_count": 1,
              "top_findings": [
                {"level": "warn", "rule": "silence", "text": "minor gap"}
              ]
            }
          }
        }
        """.data(using: .utf8)!

        let sec = try JSONDecoder().decode(SecurityResponse.self, from: secJSON)
        XCTAssertEqual(sec.localTripwireOpen, 0)
        XCTAssertEqual(sec.namespaceAudits?["primary"]?.topFindings?.first?.rule, "silence")

        let mailJSON = """
        {
          "messages": [
            {"id": "msg-1", "at": "2026-11-24T00:00:00Z", "from": "hub:storage", "to": "primary:fleet", "text": "Check drive floor"}
          ],
          "unread_by_host": {"hub": 0, "primary": 1},
          "claims": [
            {"work": "fleet-ios", "host": "primary", "provider": "agy", "note": "Building SwiftUI app"}
          ]
        }
        """.data(using: .utf8)!

        let mail = try JSONDecoder().decode(MailResponse.self, from: mailJSON)
        XCTAssertEqual(mail.messages.first?.displayText, "Check drive floor")
        XCTAssertEqual(mail.unreadByHost?["primary"], 1)
        XCTAssertEqual(mail.claims.first?.work, "fleet-ios")
    }

    func testPairingLinkValidation() throws {
        let longCode = String(repeating: "a", count: 32)
        let shortCode = String(repeating: "b", count: 16)
        let valid = try PairingLink.parse("fleet://pair?url=https%3A%2F%2Fhub.example.ts.net%3A8444&token=\(longCode)")
        XCTAssertEqual(valid.serverURL, "https://hub.example.ts.net:8444")
        XCTAssertEqual(valid.token.count, 32)

        let local = try PairingLink.parse("fleet://pair?url=http://127.0.0.1:8694&token=\(shortCode)")
        XCTAssertEqual(local.serverURL, "http://127.0.0.1:8694")

        XCTAssertThrowsError(try PairingLink.parse("https://hub.example.ts.net:8444")) { err in
            XCTAssertEqual(err as? PairingLinkError, .invalidScheme)
        }
        XCTAssertThrowsError(try PairingLink.parse("fleet://pair?url=http://hub.example.ts.net&token=\(shortCode)")) { err in
            XCTAssertEqual(err as? PairingLinkError, .insecureServerURL)
        }
        XCTAssertThrowsError(try PairingLink.parse("fleet://pair?url=https://hub.example.ts.net&token=short")) { err in
            XCTAssertEqual(err as? PairingLinkError, .invalidToken)
        }
    }

    func testFleetStoreRoundTrip() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("FleetStoreTest-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: dir) }

        let store = FleetStore(appGroup: nil, overrideDirectory: dir)
        let morning = MorningResponse(
            version: 1,
            generated: 1795474800,
            date: "2026-11-24",
            ok: true,
            counts: CountsSummary(pass: 42, warn: 1, fail: 0),
            cadence: nil,
            agyMorning: nil,
            rows: [MorningRow(key: "disk:hub", status: "pass", name: "Hub root", value: "200 GB")]
        )
        store.saveMorning(morning)
        let loaded = store.loadMorning()
        XCTAssertEqual(loaded, morning)
    }
}

private extension Optional where Wrapped == [DriveInfo] {
    var orEmpty: [DriveInfo] { self ?? [] }
}

private extension Optional where Wrapped == [HostSample] {
    var orEmpty: [HostSample] { self ?? [] }
}
