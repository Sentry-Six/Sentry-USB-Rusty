import { ExpandLessIcon, ExpandMoreIcon, NotificationsIcon } from "@/components/icons"
import { useId, useState } from "react"
import type { StepProps } from "../SetupWizard"
import { SecretInput } from "../SecretInput"
import { requiredByProvider } from "../notificationFields"
import { cn } from "@/lib/utils"

interface NotificationProvider {
  id: string
  label: string
  enableField: string
  fields: { key: string; label: string; type?: string; placeholder?: string; hint?: string; secret?: boolean }[]
}


const providers: NotificationProvider[] = [
  {
    id: "pushover", label: "Pushover", enableField: "PUSHOVER_ENABLED",
    fields: [
      { key: "PUSHOVER_USER_KEY", label: "User Key", placeholder: "user_key", secret: true },
      { key: "PUSHOVER_APP_KEY", label: "App Key", placeholder: "app_key", secret: true },
    ],
  },
  {
    id: "gotify", label: "Gotify", enableField: "GOTIFY_ENABLED",
    fields: [
      { key: "GOTIFY_DOMAIN", label: "Domain", placeholder: "https://gotify.example.com" },
      { key: "GOTIFY_APP_TOKEN", label: "App Token", placeholder: "token", secret: true },
      { key: "GOTIFY_PRIORITY", label: "Priority", placeholder: "5" },
    ],
  },
  {
    id: "discord", label: "Discord", enableField: "DISCORD_ENABLED",
    fields: [
      { key: "DISCORD_WEBHOOK_URL", label: "Webhook URL", placeholder: "https://discord.com/api/webhooks/...", secret: true },
    ],
  },
  {
    id: "telegram", label: "Telegram", enableField: "TELEGRAM_ENABLED",
    fields: [
      { key: "TELEGRAM_CHAT_ID", label: "Chat ID", placeholder: "123456789" },
      { key: "TELEGRAM_BOT_TOKEN", label: "Bot Token", placeholder: "bot123:abc...", secret: true },
    ],
  },
  {
    id: "ifttt", label: "IFTTT", enableField: "IFTTT_ENABLED",
    fields: [
      { key: "IFTTT_EVENT_NAME", label: "Event Name", placeholder: "event_name" },
      { key: "IFTTT_KEY", label: "Key", placeholder: "key", secret: true },
    ],
  },
  {
    id: "slack", label: "Slack", enableField: "SLACK_ENABLED",
    fields: [
      { key: "SLACK_WEBHOOK_URL", label: "Webhook URL", placeholder: "https://hooks.slack.com/...", secret: true },
    ],
  },
  {
    id: "signal", label: "Signal", enableField: "SIGNAL_ENABLED",
    fields: [
      { key: "SIGNAL_URL", label: "Signal CLI URL", placeholder: "http://localhost:8080" },
      { key: "SIGNAL_FROM_NUM", label: "From Number", placeholder: "+1234567890" },
      { key: "SIGNAL_TO_NUM", label: "To Number", placeholder: "+1234567890" },
    ],
  },
  {
    id: "matrix", label: "Matrix", enableField: "MATRIX_ENABLED",
    fields: [
      { key: "MATRIX_SERVER_URL", label: "Server URL", placeholder: "https://matrix.org" },
      { key: "MATRIX_USERNAME", label: "Username", placeholder: "username" },
      { key: "MATRIX_PASSWORD", label: "Password", type: "password", placeholder: "password" },
      { key: "MATRIX_ROOM", label: "Room ID", placeholder: "!roomid:matrix.org" },
    ],
  },
  {
    id: "sns", label: "AWS SNS", enableField: "SNS_ENABLED",
    fields: [
      { key: "AWS_REGION", label: "Region", placeholder: "us-east-1" },
      { key: "AWS_ACCESS_KEY_ID", label: "Access Key ID", placeholder: "AKIA...", secret: true },
      { key: "AWS_SECRET_ACCESS_KEY", label: "Secret Key", type: "password", placeholder: "secret" },
      { key: "AWS_SNS_TOPIC_ARN", label: "Topic ARN", placeholder: "arn:aws:sns:..." },
    ],
  },
  {
    id: "webhook", label: "Webhook", enableField: "WEBHOOK_ENABLED",
    fields: [
      { key: "WEBHOOK_URL", label: "Webhook URL", placeholder: "http://example.com/webhook", secret: true },
    ],
  },
  {
    id: "ntfy", label: "ntfy", enableField: "NTFY_ENABLED",
    fields: [
      { key: "NTFY_URL", label: "URL & Topic", placeholder: "https://ntfy.sh/yourtopic" },
      { key: "NTFY_TOKEN", label: "Access Token", placeholder: "optional", secret: true },
      { key: "NTFY_PRIORITY", label: "Priority", placeholder: "3" },
    ],
  },
  {
    id: "mobile_push", label: "Mobile App", enableField: "MOBILE_PUSH_ENABLED",
    fields: [],
  },
]

function isProviderEnabled(provider: NotificationProvider, data: StepProps["data"]): boolean {
  // Mobile App has no fields → its enable state must be a real stored toggle.
  if (provider.id === "mobile_push") return data[provider.enableField] === "true"
  const required = requiredByProvider[provider.enableField] ?? provider.fields.map((f) => f.key)
  return required.length > 0 && required.some((k) => (data[k] ?? "").trim() !== "")
}

function ProviderCard({ provider, data, onChange, errorFields, readOnly }: { provider: NotificationProvider; errorFields: Set<string>; readOnly?: boolean } & Pick<StepProps, "data" | "onChange">) {
  const id = useId()
  const enabled = isProviderEnabled(provider, data)
  // Default expand on enabled providers, but let the user toggle freely.
  const [expanded, setExpanded] = useState(enabled)
  const isMobile = provider.id === "mobile_push"

  return (
    <div className={cn("rounded-lg border transition-colors", enabled ? "border-blue-500/30 bg-blue-500/5" : "border-white/5 bg-white/[0.02]")}>
      <button
        type="button"
        aria-expanded={expanded}
        aria-controls={id}
        onClick={() => setExpanded(!expanded)}
        className="flex w-full items-center justify-between px-4 py-3"
      >
        <span className={cn("text-sm font-medium", enabled ? "text-slate-200" : "text-slate-400")}>
          {provider.label}
        </span>
        {expanded ? <ExpandLessIcon className="h-4 w-4 text-slate-600" /> : <ExpandMoreIcon className="h-4 w-4 text-slate-600" />}
      </button>

      {expanded && isMobile && (
        <div id={id} className="border-t border-white/5 px-4 py-3">
          <label className="flex cursor-pointer items-center gap-2">
            <input
              type="checkbox"
              checked={enabled}
              disabled={readOnly}
              onChange={(e) => onChange(provider.enableField, e.target.checked ? "true" : "false")}
              className="h-4 w-4 rounded border-white/20 bg-white/5 accent-blue-500"
            />
            <span className="text-sm text-slate-300">Enable mobile push notifications</span>
          </label>
          <p className="mt-2 text-xs text-slate-400">
            Pair your phone in Notifications → Delivery, then enter the code in the mobile app.
          </p>
        </div>
      )}

      {expanded && !isMobile && provider.fields.length > 0 && (
        <div id={id} className="grid gap-3 border-t border-white/5 px-4 py-3 sm:grid-cols-2">
          {provider.fields.map((f) => {
            const hasError = enabled && errorFields.has(f.key)
            const inputCls = cn(
              "w-full rounded-lg border bg-white/5 px-3 py-1.5 text-sm text-slate-100 placeholder-slate-600 outline-none transition focus:ring-1",
              hasError
                ? "border-red-500/50 focus:border-red-500/50 focus:ring-red-500/25"
                : "border-white/10 focus:border-blue-500/50 focus:ring-blue-500/25"
            )
            return (
              <div key={f.key}>
                <label htmlFor={`${id}-${f.key}`} className="mb-1 block text-xs font-medium text-slate-400">{f.label}</label>
                {f.secret || f.type === "password" ? (
                  <SecretInput
                    id={`${id}-${f.key}`}
                    readOnly={readOnly}
                    value={data[f.key] ?? ""}
                    onChange={(v) => onChange(f.key, v)}
                    placeholder={f.placeholder}
                    className={cn(inputCls, "pr-8")}
                  />
                ) : (
                  <input
                    id={`${id}-${f.key}`}
                    readOnly={readOnly}
                    type={f.type ?? "text"}
                    value={data[f.key] ?? ""}
                    onChange={(e) => onChange(f.key, e.target.value)}
                    placeholder={f.placeholder}
                    className={inputCls}
                  />
                )}
                {f.hint && <p className="mt-0.5 text-xs text-slate-500">{f.hint}</p>}
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}

export function NotificationsStep({ data, onChange, readOnly }: StepProps & { readOnly?: boolean }) {
  const titleId = useId()
  const missingFields = new Set<string>()
  for (const p of providers) {
    if (isProviderEnabled(p, data)) {
      for (const key of (requiredByProvider[p.enableField] ?? [])) {
        if (!data[key]?.trim()) missingFields.add(key)
      }
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <NotificationsIcon className="h-4 w-4 text-blue-400" />
        <h3 className="text-sm font-semibold uppercase tracking-wider text-slate-400">
          Push Notifications
        </h3>
      </div>

      <p className="text-xs text-slate-500">
        {readOnly ? "Provider configuration from this device." : "Fill in a provider’s fields to enable it. Clear them to disable it."}
      </p>

      <div className="mb-3">
        <label htmlFor={titleId} className="mb-1 block text-sm font-medium text-slate-300">Notification Title</label>
        <input
          id={titleId}
          readOnly={readOnly}
          type="text"
          value={data.NOTIFICATION_TITLE ?? ""}
          onChange={(e) => onChange("NOTIFICATION_TITLE", e.target.value)}
          placeholder="SentryUSB"
          className="w-full rounded-lg border border-white/10 bg-white/5 px-3 py-2 text-sm text-slate-100 placeholder-slate-600 outline-none transition focus:border-blue-500/50 focus:ring-1 focus:ring-blue-500/25"
        />
      </div>

      <div className="space-y-2">
        {providers.map((p) => (
          <ProviderCard key={p.id} provider={p} data={data} onChange={onChange} errorFields={missingFields} readOnly={readOnly} />
        ))}
      </div>

      <p className="text-xs text-slate-400">Choose alert types in Notifications → Events.</p>
    </div>
  )
}
