import { Badge } from "@/components/ui/badge"
import type { Source } from "@/lib/types"

const LABELS: Record<Source, string> = {
  standard: "Standard",
  config: "localforce.toml",
  metadata: "SFDX metadata",
  ui: "Created here",
}

/** Where an object or field is defined. */
export function SourceBadge({ source }: { source: Source }) {
  return (
    <Badge
      variant={source === "standard" ? "secondary" : "outline"}
      title={`Defined in: ${LABELS[source]}`}
    >
      {LABELS[source]}
    </Badge>
  )
}
