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
              "file": "2026-11-23.json",
              "status": "fail",
              "counts": {
                "denies": {"secrets": 2},
                "runs": 790,
                "security_runs": 7142
              },
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
        XCTAssertEqual(sec.namespaceAudits?["primary"]?.runs, 790)
        XCTAssertEqual(sec.namespaceAudits?["primary"]?.topFindings?.first?.rule, "silence")

        let mailJSON = """
        {
          "messages": [
            {"id": "msg-1", "at": "2026-11-24 07:01:05", "from": "hub", "project": "storage", "text": "Check drive floor", "read": true}
          ],
          "unread_by_host": {"hub": 0, "primary": 1},
          "unread_total": 1,
          "acked_at": "2026-11-24T00:00:00Z",
          "claims": [
            {"work": "fleet-ios", "host": "primary", "provider": "agy", "heartbeat": 1795474914, "expires": 1795503714, "note": "Building SwiftUI app"}
          ]
        }
        """.data(using: .utf8)!

        let mail = try JSONDecoder().decode(MailResponse.self, from: mailJSON)
        XCTAssertEqual(mail.messages.first?.displayText, "Check drive floor")
        XCTAssertEqual(mail.messages.first?.project, "storage")
        XCTAssertEqual(mail.messages.first?.read, true)
        XCTAssertEqual(mail.unreadByHost?["primary"], 1)
        XCTAssertEqual(mail.unreadTotal, 1)
        XCTAssertEqual(mail.ackedAt, "2026-11-24T00:00:00Z")
        XCTAssertEqual(mail.claims.first?.work, "fleet-ios")
        XCTAssertEqual(mail.claims.first?.heartbeat, "1795474914")
    }

    func testDecodeLiveShapedOverviewVariants() throws {
        let json = """
        {
          "generated_at": 1795476680,
          "date": "2026-11-24",
          "counts": {"pass": 293, "warn": 40, "fail": 23},
          "hosts": [],
          "capacity": {
            "paused": ["codex"],
            "pools": [
              {
                "engine": "codex",
                "pool": "codex:primary",
                "remaining_pct": 0,
                "paused": true,
                "unavailable": "downgraded & paused"
              }
            ]
          },
          "backups": {
            "idrive": {
              "checked_at": 1795476352,
              "host_vm": "Running",
              "container": true,
              "running": true,
              "percent": 17.08,
              "sent_bytes": 187235595029,
              "total_bytes": 1095922268785,
              "phase": "Running",
              "storage": {"used_gb": 5203.48, "total_gb": 10000.0}
            },
            "placement_violations": 1
          },
          "security": {"status": "pass"},
          "unread_mail": 4,
          "active_claims": 1
        }
        """.data(using: .utf8)!

        let ov = try JSONDecoder().decode(FleetOverviewResponse.self, from: json)
        XCTAssertEqual(ov.capacity.pools.first?.unavailable, true)
        XCTAssertEqual(ov.capacity.pools.first?.unavailableReason, "downgraded & paused")
        XCTAssertEqual(ov.backups.idrive?.container, "true")
        XCTAssertEqual(ov.backups.idrive?.storage, "5.20 TB / 10.00 TB")

        let now = Date(timeIntervalSince1970: 1795476680)
        XCTAssertEqual(FleetFormatter.relativeTime(fromTimestamp: "1795476620", now: now), "1m ago")
        XCTAssertNotEqual(FleetFormatter.relativeTime(fromTimestamp: "2026-11-24 07:01:05", now: now), "2026-11-24 07:01:05")
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

    func testPresentationHelpersAndFilters() throws {
        // shortJobName & formatCompactTokens
        XCTAssertEqual(shortJobName("com.example.agent-mail-relay"), "agent-mail-relay")
        XCTAssertEqual(shortJobName("org.vendor.air.web-refresh"), "air.web-refresh")
        XCTAssertEqual(shortJobName("single"), "single")
        XCTAssertEqual(FleetFormatter.formatCompactTokens(6_312_755_650), "6.3B tok")
        XCTAssertEqual(FleetFormatter.formatCompactTokens(265_194_540), "265.2M tok")
        XCTAssertEqual(FleetFormatter.formatCompactTokens(45_000), "45K tok")

        // Disk threshold + host headline verdict
        let criticalJSON = """
        {"id":"laptop","ok":true,"disk_free_gb":13.4,"disk_total_gb":245.1,"disk_target_gb":40.0,"disk_floor_gb":20.0,"failing_jobs_count":0}
        """.data(using: .utf8)!
        let criticalHost = try JSONDecoder().decode(HostSummary.self, from: criticalJSON)
        let critDisk = diskThresholdInfo(for: criticalHost)
        XCTAssertEqual(critDisk.state, .critical)
        XCTAssertTrue(critDisk.thresholdSummary.contains("below 20 GB floor"))
        XCTAssertEqual(hostHeadlineVerdict(for: criticalHost).label, "Low Disk")

        let healthyJSON = """
        {"id":"hub","ok":true,"disk_free_gb":134.0,"disk_total_gb":494.4,"disk_target_gb":60.0,"disk_floor_gb":40.0,"failing_jobs_count":0}
        """.data(using: .utf8)!
        let healthyHost = try JSONDecoder().decode(HostSummary.self, from: healthyJSON)
        XCTAssertEqual(diskThresholdInfo(for: healthyHost).state, .healthy)
        XCTAssertEqual(hostHeadlineVerdict(for: healthyHost).label, "Healthy")

        // Attention clusters & Board grouping
        let rows = [
            MorningRow(key: "disk:laptop", status: "fail", area: "disk", name: "laptop internal", value: "13.4 GB"),
            MorningRow(key: "job:hub:yt", status: "fail", area: "job", name: "hub yt-watch"),
            MorningRow(key: "place:arch", status: "fail", area: "place", name: "Archive set"),
            MorningRow(key: "backup:home", status: "warn", area: "backup", name: "Home mirror"),
            MorningRow(key: "proj:app", status: "pass", area: "project", name: "App check"),
        ]
        let clusters = buildAttentionClusters(rows: rows)
        XCTAssertEqual(clusters.map(\.id), ["disk", "job", "placement"])

        let issues = filterAndSortRows(rows, filter: .issues)
        XCTAssertEqual(issues.count, 4)
        XCTAssertEqual(filterAndSortRows(rows, filter: .backups).map(\.key), ["backup:home"])
        XCTAssertEqual(filterAndSortRows(rows, filter: .projects).map(\.key), ["proj:app"])

        let groups = groupRowsByArea(issues)
        XCTAssertEqual(groups.map(\.id), ["disk", "job", "place", "backup"])

        // Capacity partitioning
        let pools = [
            CapacityPool(engine: "claude", pool: "5h", paused: true, unavailable: true),
            CapacityPool(engine: "gemini", pool: "week", remainingPct: 94),
            CapacityPool(engine: "gemini", pool: "5h", remainingPct: 77),
        ]
        let part = partitionCapacityPools(pools)
        XCTAssertEqual(part.activeGroups.count, 1)
        XCTAssertEqual(part.activeGroups.first?.pools.map { displayPoolName($0) }, ["5-hour", "Weekly"])
        XCTAssertEqual(part.pausedPools.count, 1)

        // Morning brief parsing
        let brief = """
        1. Top priorities today
        - **Disk low**: clear caches on laptop.
        - **Mirror blocked**: destination below floor.

        2. Notes
        Everything else nominal.
        """
        let sections = parseMorningBrief(brief)
        XCTAssertEqual(sections.count, 2)
        XCTAssertEqual(sections[0].title, "Top priorities today")
        XCTAssertEqual(sections[0].bullets.first?.lead, "Disk low")
        XCTAssertEqual(sections[0].bullets.first?.body, "clear caches on laptop.")

        // GDrive breakdown & mirror state
        let detail = GDriveDetail(total: 3, activeRuns: 1, okCount: 1, items: [
            GDriveItem(name: "a", running: false, lastAttempt: nil, lastOK: 1, lastResult: "ok"),
            GDriveItem(name: "b", running: true, lastAttempt: nil, lastOK: nil, lastResult: nil),
            GDriveItem(name: "c", running: false, lastAttempt: nil, lastOK: nil, lastResult: "not mounted"),
        ])
        XCTAssertEqual(gdriveBreakdown(detail: detail, summary: nil), "1 OK · 1 running · 1 unmounted")
        let mirror = cleanMirrorState("skipped: on battery")
        XCTAssertTrue(mirror.isSkipped)
        XCTAssertEqual(mirror.reason, "on battery")

        // Interactive action helpers & snooze filtering
        let snoozedRows = [
            MorningRow(key: "job:hub:air", status: "fail", area: "job", name: "hub air.web-refresh", detail: "org.vendor.air.web-refresh; when every 3h; exit 1"),
            MorningRow(key: "disk:laptop:internal", status: "fail", area: "disk", name: "laptop internal", value: "13.4 GB"),
            MorningRow(key: "storage:hub:free", status: "fail", area: "disk", name: "hub free space", snoozedUntil: "2026-11-25T00:00:00Z"),
            MorningRow(key: "place:arch", status: "fail", area: "place", name: "Archive", detail: "0 encrypted copy"),
        ]
        XCTAssertEqual(filterAndSortRows(snoozedRows, filter: .issues).count, 3)
        XCTAssertEqual(filterAndSortRows(snoozedRows, filter: .failing).count, 3)
        XCTAssertEqual(filterAndSortRows(snoozedRows, filter: .snoozed).map(\.key), ["storage:hub:free"])
        XCTAssertFalse(buildAttentionClusters(rows: snoozedRows).map(\.summary).joined(separator: " ").contains("hub free"))

        let jt = extractJobTarget(from: snoozedRows[0])
        XCTAssertEqual(jt?.host, "hub")
        XCTAssertEqual(jt?.label, "org.vendor.air.web-refresh")

        let recDisk = recommendedHostAction(for: snoozedRows[1], actionHosts: ["hub", "primary"], knownHostIDs: ["hub", "primary", "laptop"])
        XCTAssertNil(recDisk)

        let recAllowedDisk = recommendedHostAction(for: snoozedRows[2], actionHosts: ["hub", "primary"], knownHostIDs: ["hub", "primary", "laptop"])
        XCTAssertEqual(recAllowedDisk?.host, "hub")
        XCTAssertEqual(recAllowedDisk?.kind, "free-space-guard")

        let recPlace = recommendedHostAction(for: snoozedRows[3], actionHosts: ["hub", "primary"])
        XCTAssertEqual(recPlace?.host, "hub")
        XCTAssertEqual(recPlace?.kind, "placement-check")

        XCTAssertTrue(suggestedFixPrompt(for: snoozedRows[0]).contains("org.vendor.air.web-refresh"))
    }
}

private extension Optional where Wrapped == [DriveInfo] {
    var orEmpty: [DriveInfo] { self ?? [] }
}

private extension Optional where Wrapped == [HostSample] {
    var orEmpty: [HostSample] { self ?? [] }
}
