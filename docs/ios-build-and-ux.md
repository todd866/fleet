# Building and checking the iPhone apps

Use the app's existing build command and XcodeGen spec. The build helper owns simulator selection, scheduling and artifact paths. Keep platform-specific work such as a Rust keyboard core or Metal archive in the owning repo. Check the app's instructions for the current command.

## Device builds and distribution

Fleet's reference commands are `bin/fleet-ios test`, `bin/fleet-ios publish-ipa --no-upload` for a local export and `bin/fleet-ios install-device` for an authorised installation. Mission Control retains its own `bin/agy-control-ios test` and `build-device` commands. Keep signing and server overrides in the local configuration.

Retry provisioning with the same requested capabilities. A cached profile must satisfy those capabilities too. Check signatures on the archive and again after distribution export, since export can re-sign the product. An app that reads shared data through an extension needs the configured App Group on both signatures. To check an existing Fleet artifact from the Fleet checkout, run

    bin/ios-verify-app-group /path/to/Fleet.app --extension FleetWidgets.appex --group-info-key FleetAppGroup

This helper reads the runtime group from each target's Info.plist. For another app, supply its key and the extension names that use the group. A passing signature check establishes what the binaries contain. Device provisioning and a working app-to-extension data exchange have their own installation and functional checks.

Fleet serialises build operations per checkout and starts each device archive in a fresh directory. A validated archive and its dSYMs remain at `ios/build/Fleet.xcarchive`. Its `fleet-build.json` records the source revision, dirty flag, build number and checks. After a successful Apple export it also records the SHA-256 of `ios/build/Fleet.ipa`. A failed export leaves the previous IPA in place. Match its digest to the receipt before referring to it as this build. A dirty flag means local inputs also contributed and the commit alone cannot reproduce them.

## Phone UX acceptance

Pick the action someone opens this app to finish. Start from a realistic fixture, use the visible controls, and assert the specific result. For a reply, assert the expected thread and exactly one stored message. For a saved track, relaunch with the server unavailable and check the selected recording plays from the local file with silent audio QA. For typing, assert the expected text including spacing. Keep account changes, personal media and real outgoing messages out of these fixtures.

For an XCUITest journey, retain the run's xcresult and check the exact case. From the Fleet checkout run

    bin/ios-check-journey /path/to/run.xcresult 'Suite/testMethod()'

Inspect light and dark phone captures with the actual content. For input screens check the send or save action with the keyboard open. For dense shelves check a narrow viewport, enlarged text, missing artwork and similar long titles. Keep the full title reachable by touch and VoiceOver. Record the simulator model and OS with the result. Use the existing phone control tool for device-specific behavior such as installing a keyboard or starting hardware playback.

Report which revision was built, which journey passed, and where it ran. Record installation or distribution only when observed. Keep private captures and test bundles on the machine that ran the test.
