# Notifications

Sentry USB can send you push notifications when:

- Archiving starts, succeeds or fails
- New drives have been mapped
- Temperature, storage or RTC battery warnings need attention
- A keep-awake command fails, or accessory-power automation changes state
- An update is available or music has been synchronized

Configure providers in the [Setup Wizard](Setup-Wizard-Guide#8-notifications), or anytime later under **Settings** → **Notifications**.

You can enable as many providers as you want at once.

## Messages and history

All supported providers receive the same short event summary, including generic
webhooks and Sentry Connect. Open **Notifications → History → Details** in the
web interface for the full message, exact timestamp and provider error details.
Archive logs retain the operational diagnostics. Credentials are removed from
provider errors; very large provider responses may be truncated with an explicit
marker.

Existing history and notification settings are retained. Older app versions can
still read the full message; expanding details requires an app version with the
updated history view. No simultaneous Pi/app upgrade is required.

The archive count refers to the current batch of pending files. Previous progress
checkpoints are not subtracted again. Drive mapping summaries distinguish drives
mapped earlier from those mapped in the current pass; an unavailable archive does
not necessarily mean the car was away from home.

Webhook and IFTTT payload fields are unchanged, but their human-readable message
text is shorter. Avoid using exact message wording as an automation protocol.
Custom local start/finish notification commands keep their existing arguments and
environment. A provider accepting a notification does not establish that a device
received it or a person read it.

## Providers

### Pushover
Paid one-time-fee iOS / Android app. Most reliable for personal alerts.

| Field | Where to get it |
|-------|-----------------|
| User Key | Pushover dashboard → your user key |
| App Key | Create an Application in the Pushover dashboard |

### ntfy
Free, self-hostable push notifications. Subscribe to a topic on the [ntfy.sh](https://ntfy.sh) iOS / Android app.

| Field | Example |
|-------|---------|
| URL & Topic | `https://ntfy.sh/your-secret-topic` |
| Access Token | (optional, only for self-hosted with auth) |
| Priority | `3` (1=lowest, 5=highest) |

### Gotify
Self-hosted push notification server. You run Gotify on a home server; the Android app receives the push.

| Field | Example |
|-------|---------|
| Domain | `https://gotify.example.com` |
| App Token | Created in the Gotify web UI under Apps |
| Priority | `5` |

### Discord
Posts to a Discord channel via a webhook.

| Field | Where to get it |
|-------|-----------------|
| Webhook URL | Server Settings → Integrations → Webhooks → New Webhook → Copy URL |

### Telegram
Posts via a Telegram bot.

| Field | Where to get it |
|-------|-----------------|
| Chat ID | Send a message to your bot, then visit `https://api.telegram.org/bot<TOKEN>/getUpdates` |
| Bot Token | Create a bot with [@BotFather](https://t.me/botfather) |

### Slack
Posts to a Slack channel via an Incoming Webhook.

| Field | Where to get it |
|-------|-----------------|
| Webhook URL | Slack App settings → Incoming Webhooks → Add to channel |

### Signal
Sends via [signal-cli](https://github.com/AsamK/signal-cli) running on your network.

| Field | Example |
|-------|---------|
| Signal CLI URL | `http://signal-host:8080` |
| From Number | `+15555550100` |
| To Number | `+15555550199` |

### Matrix
Posts to a Matrix room.

| Field | Example |
|-------|---------|
| Server URL | `https://matrix.org` |
| Username | `yourname` |
| Password | _your password_ |
| Room ID | `!roomid:matrix.org` |

### AWS SNS
For automation / cloud workflows.

| Field | Example |
|-------|---------|
| Region | `us-east-1` |
| Access Key ID | `AKIA...` |
| Secret Key | _your secret_ |
| Topic ARN | `arn:aws:sns:us-east-1:123:my-topic` |

### IFTTT
Triggers an IFTTT applet by sending an event.

| Field | Where to get it |
|-------|-----------------|
| Event Name | Whatever you named the event in your applet |
| Key | IFTTT Webhooks service → Documentation page → your key |

### Webhook
Generic — POSTs a JSON payload to any URL. Useful for Home Assistant, n8n, Node-RED, etc.

| Field | Example |
|-------|---------|
| Webhook URL | `http://homeassistant.local:8123/api/webhook/sentryusb` |

### Mobile App (beta)
Push notifications to the Sentry USB iOS companion app. Currently in beta. Toggle it on in the wizard if you've installed the app and paired it.

---

## Testing notifications

The **Send Test** action for the paired mobile app sends a Sentry Connect test
notification. It does not test the other configured providers. Their delivery
results appear in notification history when an enabled event is sent.
