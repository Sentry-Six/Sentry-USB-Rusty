# Notifications

Sentry USB can send you push notifications when:

- Archiving starts, succeeds or fails
- New drives have been mapped
- Temperature, storage or RTC battery warnings need attention
- A keep-awake command fails, or accessory-power automation changes state
- An update is available or music has been synchronized

Configure providers in the [Setup Wizard](Setup-Wizard-Guide#8-notifications), or anytime later on the **Notifications** page → **Delivery** tab (choose which events alert you on the **Events** tab).

You can enable as many providers as you want at once.

## Messages and history

Every provider gets the same short summary of each event. For the full message, the exact time and any delivery error, open **Notifications → History** and tap **Details**.

Message wording can change between versions. If you automate with a webhook or IFTTT, use the payload fields rather than matching the message text.

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

### Mobile App
Push notifications to the [Sentry Connect](https://sentry-six.com/sentry-connect/) app. Turn on **Enable mobile push notifications** in the wizard. Then, in Sentry Connect, connect to your Pi, open its **Notifications** settings and pair your phone. The app pairs itself, so there's no code to type. The Pi needs internet while pairing.

Paired phones appear in the web UI under **Notifications → Delivery → Mobile Notifications**, where you can remove one or send a test.

---

## Testing notifications

**Send Test Notification** under **Notifications → Delivery → Mobile Notifications** sends a test to your paired phones only. Other providers show their results in **Notifications → History** the next time an enabled event is sent.

For update notifications and the automatic-update option, see [Updates](Updates).
