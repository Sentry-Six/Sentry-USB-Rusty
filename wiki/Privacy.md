# Privacy

This page documents every outbound data flow from a SentryUSB device,
the reporting controls, how long the data is retained,
and how to disable it. If anything you observe on the wire doesn't
match what's listed here, it's a bug — please open an issue.

## Summary

New installations preselect **Yes, count me** on the Privacy step for minimal
device statistics. No reporting starts from that default until the user
continues past the step and the choice is saved. **No thanks** prevents the
first report. Cancelling or going Back does not save the draft selection.

Existing installations keep their saved choice. Existing opt-outs and existing
installations without a recorded choice remain off. The reporting setting in
`Settings → System → Device counting` controls later changes. Read the
[Privacy Policy](https://sentry-six.com/privacy) for the legal disclosures.

## Per-flow disclosure

### 1. Device count and current version (optional)

- **Endpoint:** `POST https://api.sentry-six.com/sentryusb-rusty/telemetry`
- **When:** After startup, after enabling analytics, and daily while enabled.
  Offline attempts retry with bounded backoff. Repeat reports update the same
  device record.
- **Sent:** `fingerprint` (salted SHA-256 device ID), `current_version`, and
  `report_kind: running_version` (protocol label).
  No raw hardware serial, board model, boot ID, architecture, files, locations,
  or update download/availability data is included. Normal source-IP connection metadata
  is visible to the server and briefly used for rate limiting.
- **Identity:** The ID derives from the board serial and is stable across
  reinstalls on Raspberry Pi. Devices without a readable hardware serial are
  not counted; an installation ID is never used as a substitute.
- **Purpose:** Count known and recently active devices and show their latest
  reported running versions. The server keeps just the device ID, latest
  version, and first/last report times. It does not store version history or
  send automatic per-device notifications to Discord. Authorized maintainers
  can view the minimal records through restricted administrative tools,
  including Discord commands. Reports describe the running version,
  not an offered or downloaded update.
- **Control:** New installations show Yes preselected on the Privacy step,
  with an equally accessible No. The selection is saved only when continuing
  past that step. Existing settings are preserved; existing installations with
  no choice stay off. Both options provide the same core features and updates.
- **Retention:** The four-field device summary remains until deleted or
  deletion is requested. Opting out stops future reports; it does not erase
  the existing summary. No report-ID receipts or version-history rows are kept.
- **How to disable:** `Settings → System → Device counting`. To request
  deletion of stored analytics, contact `privacy@sentry-six.com`.

### 2. Update checks and retired install reporting

GitHub release checks still work with reporting disabled. Current Rusty
versions do not send update-check telemetry or anonymous install beacons to
Sentry Six. The legacy `/sentryusb/telemetry` and `/sentryusb/install-beacon`
endpoints discard reports from old clients; they do not feed the new Rusty
device count.

### 3. Wraps / lock chime submissions

- **Endpoint:** `POST https://api.sentry-six.com/wraps/upload`,
  `POST https://api.sentry-six.com/lockchime/upload`
- **Sent:** The file you uploaded (plus an optional preview for wraps), display
  name, model and original filename for wraps, file size, audio duration for
  lock chimes, and your source IP. The server also generates a submission code
  and records review status and timestamps.
- **Identifier:** **No device fingerprint.** Older versions sent an
  `X-Fingerprint` header — that was removed. Abuse handling now goes
  through the Discord moderation queue plus per-IP rate limits.
- **Purpose:** Accepting, reviewing, and publishing your contribution, plus
  proportionate rate limiting, abuse investigation, and moderation.
- **Legal basis:** Contractual necessity under Art. 6(1)(b) for accepting,
  reviewing, and publishing the contribution you requested; legitimate
  interests under Art. 6(1)(f) for proportionate rate limiting, abuse
  investigation, and moderation.
- **Retention:** Pending and approved asset files remain until declined or
  manually deleted. Declining removes the asset file, but the submission row,
  including its source IP, status, and review metadata, has no fixed automatic
  deletion period and remains until manually removed or deletion is requested.
  The source IP is not exposed in the public library view.
- **How to disable:** Don't submit. Browsing/downloading the library
  sends no custom or device identifier; the source IP is necessarily seen
  and briefly used for rate limiting.

### 4. Wraps / lock chime downloads

- **Endpoint:** `GET https://api.sentry-six.com/wraps/download/<code>`,
  `GET https://api.sentry-six.com/lockchime/download/<code>`
- **Sent:** Standard HTTP request with no custom or device identifier. The
  source IP is necessarily seen and briefly used for rate limiting.
- **Identifier:** None is sent by current SentryUSB versions. Older versions
  may still send a legacy `X-Fingerprint` header; the current client does not.
- **Purpose:** Fetch the requested asset.
- **Legal basis:** Contractual necessity — you asked for the file.
- **Retention:** Current-client source-IP rate-limit entries remain only
  briefly in memory. Legacy per-asset fingerprint download records created by
  older clients may remain until manually deleted; contact
  `privacy@sentry-six.com` to request deletion.
- **How to disable:** Don't download.

### 5. Sentry Cloud (sync feature, opt-in)

- **Endpoint:** Various `https://api.sentry-six.com/cloud/...` routes.
- **Sent:** Your Sentry Cloud account credentials when signing in, followed by
  encrypted drive-history and telemetry data you choose to sync. Sentry Cloud
  does not upload dashcam video.
- **Identifier:** Your Sentry Cloud account.
- **Purpose:** Cloud sync requires it — the feature can't function
  otherwise.
- **Legal basis:** Contractual necessity (Art. 6(1)(b)) — you signed
  up for the service.
- **Retention:** See the Sentry Cloud terms of service.
- **How to disable:** Don't sign in to Cloud. The feature is fully
  opt-in.

### 6. iOS push notification pairing (opt-in)

- **Endpoint:** `POST https://notifications.sentry-six.com/register-code`
- **Sent:** A `device_id` (random UUID generated on this Pi),
  `device_secret`, your chosen pairing code, and your Pi's hostname.
- **Identifier:** The `device_id` — but it's a random value created
  locally on first run, **not** derived from your hardware. Resetting
  it generates a new one.
- **Purpose:** Routing push notifications from your Pi to your phone.
- **Legal basis:** Consent — you actively enabled this feature.
- **Retention:** Kept until you unpair the device.
- **How to disable:** Don't pair, or unpair in the iOS app + delete
  the credentials on the Pi.

### 7. AI Support & Help (user-initiated)

- **Endpoint:** Your browser talks only to this Pi at
  `/api/support/ai/...`. The Pi proxies the fixed request types to
  `https://api.sentry-six.com/ai-support/...`. The local browser-to-Pi hop
  normally uses HTTP, so chat content and access tokens are not encrypted on
  that local hop. Use AI Support only from a trusted local network. The Pi's
  onward connection to the public API uses HTTPS.
- **Sent when you start a conversation:** The text you enter, a random
  conversation/request identifier, the displayed disclosure version,
  the installed Sentry USB software version, SBC model, a bounded non-secret
  support snapshot, and the fixed product ID `sentry-usb-rusty`. The snapshot
  contains Dashcam Size, Travel Mode flags, archive category toggles, and
  whether Wraps and Lock Chimes are enabled. The Pi does **not** send archive
  destinations, network names, usernames, passwords, tokens, a hardware
  fingerprint, Pi login credential, or knowledge for another product.
- **AI processing:** Relevant conversation content is processed by
  Ollama Cloud to generate a response. Ollama states that Cloud prompts
  and responses are processed transiently, are not retained beyond the
  request, and are not used to train models. Processing may occur outside
  Canada, including in the United States. The app identifies this as an
  online AI service; the model is not running solely on your Pi.
- **Server logging:** A redacted transcript of messages and responses,
  timestamps, product/version context, action decisions, and limited
  operational metadata is logged on Sentry Six servers. Authorized
  maintainers may review it to troubleshoot failures, find hallucinations,
  and improve support quality.
- **Identifier and local resume data:** To resume a chat, your browser stores
  the random conversation ID and raw random access token in local storage.
  The Sentry Six backend stores only a one-way hash of that token. The public
  IP seen from the Pi's connection is processed separately for rate limiting
  and abuse prevention: raw values remain only in short-lived in-memory rate
  buckets (up to about two hours), while a gateway-keyed one-way hash and
  daily diagnostic-upload counters may remain for up to about 49 hours and
  are not linked to the transcript or diagnostic text. None of these values
  is derived from your Pi hardware. Clearing browser
  site data removes local access but does not delete the server copy; use
  **New chat** first if you want the current conversation deleted immediately.
- **Purpose and legal basis:** Processing the message needed to answer your
  support request is necessary to provide the AI Support service you asked
  for (Art. 6(1)(b)). Bounded, redacted transcript retention and authorized
  review for security, reliability, hallucination detection, and prompt or
  knowledge-safety improvements rely on our legitimate interests
  (Art. 6(1)(f)). You may object to that processing or request deletion at
  `privacy@sentry-six.com`, subject to applicable law. The pre-chat screen is
  an acknowledgement of these disclosures, not bundled consent for every
  processing purpose.
- **Retention:** Redacted conversation transcripts expire 90 days after
  the last activity. You can delete the current conversation immediately
  in the UI, or request deletion at `privacy@sentry-six.com`. Deletion removes
  messages and uploaded files immediately. A non-content receipt containing
  the conversation ID, one-way hashes of the access token and deletion
  idempotency key, and deletion time remains available solely for safe retries
  and replay prevention until it expires 24 hours after deletion. The expired
  receipt is removed during the next scheduled cleanup sweep, normally within
  about one additional hour.
- **How to disable:** Do not start an AI Support conversation. The rest of
  SentryUSB continues to work without it.

#### Diagnostic-file requests

The assistant cannot browse the Pi or upload a file by itself. The first
supported request type is a predefined SentryUSB diagnostics report. It
collects the date, hostname, uptime, software/OS and board details, storage
and USB-gadget state, local network addresses, service state, temperatures,
and limited tails from designated system and SentryUSB logs, including recent
`archiveloop.log` entries. It does not search arbitrary files. Log excerpts can incidentally contain local
addresses, device or vehicle identifiers, error payloads, or location-related
details; those are not separate fixed fields requested from the vehicle.

If the assistant asks for the report, the UI shows the exact file, reason,
maximum size, destination, and seven-day retention period. Nothing is
generated or uploaded until you click **Generate & upload diagnostics once**
for that specific request. That click creates a short-lived, single-use
upload token; it is not standing permission for future files. You may deny
the request and continue chatting.

The approved report leaves your Pi, is uploaded to the Sentry Six backend,
and relevant content may be processed transiently by Ollama Cloud and
reviewed by authorized maintainers. The backend accepts only the expected
diagnostics request, plain UTF-8 text with control characters removed, and a
maximum of 2 MiB. The file is deleted automatically after seven days. Review
the disclosed categories before approving, and do not approve an upload if
the report may contain passwords, tokens, private keys, precise location, or
third-party data you are not authorized to share.

#### Optional Discord help

AI Support may suggest the Sentry Six Discord for community or human help.
Opening the Discord link does not automatically send the AI transcript or
uploaded diagnostics to Discord. Anything you choose to post there is a
separate disclosure to Discord and is governed by Discord's policies.

## Things SentryUSB does **not** do

- Enable reporting on an existing installation merely because it updates.
- Send the first report for a preselected new-install choice before it is saved by continuing past Privacy.
- Send device analytics at startup or in the background when opted out.
- Send "diagnostics" or "crash reports" in the background. If a crash
  reporter is ever added, it will be its own opt-in.
- Let AI Support browse files or treat one approval as permission for a
  later upload.
- Let the Rusty UI choose another product's AI prompt or knowledge base.
- Treat the device-statistics setting as permission to upload diagnostics or
  files. Each such upload still requires its own affirmative approval.

## Source code references

If you want to verify any of the above against the source:

- Device analytics: `crates/api/src/device_reporting.rs`. The saved reporting setting is checked
  before creating or sending reports; the startup version is captured before
  the updater or API starts. Only tagged release builds embed a release tag;
  other builds capture the installed version once at startup.
- Preference wake-up: `crates/api/src/preferences.rs`. Reporting is nudged only
  after a privacy choice is successfully saved.
- Wraps/chimes header forwarding: `crates/api/src/community.rs` →
  `forward_headers()`. Should only forward `x-passcode`, never
  `x-fingerprint`.
- Notification pairing: `crates/api/src/notifications.rs` →
  `register_code_with_backend()`. Confirm the request body has no
  `fingerprint` field.

- AI Support proxy and product lock: `crates/api/src/support.rs`. Confirm
  `AI_PRODUCT_ID` is `sentry-usb-rusty`, later requests remove product
  selectors, only the browser-supplied AI conversation token and idempotency
  key are forwarded, and the trusted product header is injected by the Pi.
- AI Support disclosure acknowledgement and separate upload consent:
  `web/src/components/support/AISupportChat.tsx` and
  `web/src/api/support.ts`.

## Reporting a privacy bug

Open an issue at
[github.com/Sentry-Six/Sentry-USB-Rusty/issues](https://github.com/Sentry-Six/Sentry-USB-Rusty/issues)
or email `privacy@sentry-six.com`. If the bug is "the client sent X
even though the docs said it wouldn't" please include a `tcpdump` or
the relevant journalctl line so we can fix it.
