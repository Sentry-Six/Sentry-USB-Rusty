import { BackHandIcon, BoltIcon, VisibilityIcon, WarningIcon } from "@/components/icons"

const EVENT_REASONS: Record<string, { label: string; icon: typeof BoltIcon }> = {
  sentry_aware_object_detection: { label: "Object Detected", icon: VisibilityIcon },
  vehicle_auto_emergency_braking: { label: "Emergency Braking", icon: WarningIcon },
  user_interaction_dashcam_icon_tapped: { label: "Manual Save", icon: BackHandIcon },
  user_interaction_dashcam_panel_save: { label: "Manual Save", icon: BackHandIcon },
  user_interaction_dashcam_launcher_action_tapped: { label: "Manual Save", icon: BackHandIcon },
  user_interaction_honk: { label: "Honk", icon: BoltIcon },
  sentry_aware_accel: { label: "Acceleration", icon: BoltIcon },
  collision: { label: "Collision", icon: WarningIcon },
  user_interaction_dashcam: { label: "Manual Save", icon: BackHandIcon },
}

/** Plain words and an icon for event.json's `reason`. */
export function formatEventReason(reason: string): { label: string; Icon: typeof BoltIcon } {
  const mapped = EVENT_REASONS[reason]
  if (mapped) return { label: mapped.label, Icon: mapped.icon }
  return {
    label: reason.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase()),
    Icon: BoltIcon,
  }
}

export const CAMERA_LABELS: Record<string, string> = {
  front: "Front",
  back: "Rear",
  left_repeater: "Left Repeater",
  right_repeater: "Right Repeater",
  left_pillar: "Left Pillar",
  right_pillar: "Right Pillar",
}

export const CATEGORIES = ["RecentClips", "SavedClips", "SentryClips"] as const
