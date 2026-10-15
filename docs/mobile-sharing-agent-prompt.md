# Prompt for the Monolith mobile implementation agent

Implement app/Wi-Fi and Bluetooth song sharing in the active Flutter Monolith app. The desktop
Windows implementation now lives in Monolith Desktop 0.6.0; the earlier
design-only hold was lifted on 2026-10-11. This prompt is a handoff, not evidence
that the mobile bridge or physical interoperability has been built.

Read the mobile AGENTS/CLAUDE instructions, active import/playback/library callers,
and the desktop `docs/nearby-music.md`, `docs/bluetooth-wire-fixtures.json`,
`src/nearby.rs`, `src/lan.rs` and `src/bluetooth.rs` completely. The desktop repo remains private;
use local copies if GitHub is inaccessible, never embed repository credentials.
Apply ponytail, preserve accessibility and the Monolith visual identity. Do not
add runtime packages or download more than 100 MB without user authorization.

First implement Android↔Windows using a native Kotlin bridge through Flutter
MethodChannel/EventChannel. Use Android's secure
`listenUsingRfcommWithServiceRecord("Monolith Music", UUID)` and
`createRfcommSocketToServiceRecord(UUID)` with UUID
`e8793c34-7c2e-49ad-88c2-16c9a0762c91`. Never use insecure RFCOMM. Read official
Android Bluetooth permission rules for the targeted SDK; ask for runtime
BLUETOOTH_CONNECT and only request SCAN if actual discovery requires it. Prefer
already-paired devices, do not enable global discoverability without an explicit
user action. Keep every socket, hash, file operation and import off the UI thread.

Match wire v1 exactly: 4-byte unsigned big-endian JSON length, bounded UTF-8 JSON,
exact hello, validated one-song offer, explicit accept/decline, exact-size raw
audio (no chunk headers), then SHA-256 saved receipt only after import succeeds.
Fixtures are authoritative and their illustrative digest must never count as
valid audio. Read full streams even when writes/reads fragment headers/payloads.
Use secure pairing, 60-second consent timeout, 75-second idle timeout, 15-minute
absolute connection expiry and max 100,000,000 bytes per song. Closing a socket
cancels blocked native operations. No background receiving or unrequested server.

Send only explicit library choices. Android content URIs need ContentResolver
streams, not guessed file paths. Hash an actual stable audio snapshot if a source
cannot be reopened reliably; preserve original bytes and embedded metadata/art.
Stage incoming bytes privately, check size/hash/content and safe basename, publish
without overwriting through the platform storage APIs, then use the existing
importer. Decline/cancel/error must remove only session-owned temporary files.
If publication succeeds and import fails, retain verified audio and report import
failure without a success receipt. Do not expose arbitrary filesystem paths or
silently overwrite existing files. UUID and hello identify compatibility, not app
authenticity; recipient consent plus OS authenticated encryption provide trust.

UI: song action → Share music → paired-device picker → Send. Receive starts a
five-minute foreground session and asks before accepting each song. Show title,
paired sender label, filename, actual size/progress, Cancel, precise missing-radio
and permission-denied states, and imported-vs-transfer-only outcomes. Keep playback
responsive and avoid fake peers/progress. Sidecar images are deferred in v1.

iPhone cannot be claimed supported by reusing Android RFCOMM. Design a separate
Core Bluetooth/GATT adapter only after confirming desktop compatibility and user
scope. Linux BlueZ is also pending on desktop. Do not implement Wi-Fi-only sharing
and call it Bluetooth. Monolith app/Wi-Fi is the separate TLS transport described below.

Run wire fixtures, malformed/oversized/traversal, collision, partial/disconnect,
decline/cancel, hash mismatch, no-permission/no-radio and native lifecycle tests.
Then prove real Android↔Windows transfer/import/playback with embedded artwork,
repeated offers and listener removal on Close. Report exactly which device pairs
were tested. Do not claim interoperability based on mocks. No paid actions,
public publication, credential uploads or repository-history changes.

## Monolith app/Wi-Fi and batch selection

Add a Share chooser with **Monolith app** and **Bluetooth**, plus selection mode
with checkboxes, a selected count, select-all-visible, bulk sharing, playlist
addition/creation and confirmed removal that preserves original audio files.
Selections use persistent track IDs, including across filters/sorts. Cap shared
batches at 100 songs; one TLS/RFCOMM connection and approval/receipt per song.
Report partial completion, cancellation and destination collisions honestly.

Implement the exact UDP discovery and **raw framed protocol over TLS 1.3** from
`docs/nearby-music.md`. Do not send HTTP or assume LocalSend interoperability.
Recipient advertises only during the five-minute foreground Receive session.
The user requires sharing to stay off until Share is opened. Do not advertise,
scan, bind sockets or run a sharing service at app startup/in the background.
Close/Cancel must stop discovery and receiving, including pending I/O.
Follow the LocalSend experience: automatically discover Monolith receivers,
choose a device and ask the recipient to accept. Do not add verification-code
copying, QR-code requirements or certificate fields to the user flow. Discovery
replies carry the bounded generated alias and public SHA-256 DER certificate
fingerprint; TLS matches that selected endpoint's fingerprint and verifies its
handshake signature before song bytes. Never use an accept-all TLS callback.
No private key is broadcast or committed. Discovery is unauthenticated and
cannot establish a verified personal identity; retain explicit recipient consent
and use trusted local networks. Preserve stable address/certificate device IDs
across automatic list refreshes so a disappearing device cannot reroute Send.
Add sharing to each relevant tab's three-step guide, spotlighting Share/Select.

Check existing Flutter/Dart/native TLS and hashing capabilities before adding
packages. Android may need a native TLS server and ephemeral certificate/key
generation; iOS requires the appropriate Local Network usage explanation and
permission. Verify current platform APIs and SDK permission rules from official
docs. Windows, Linux and mobile software/hardware compatibility must each be
tested rather than inferred. Desktop loopback TLS/discovery and two real-song
imports passed; real separate devices are still unverified.
